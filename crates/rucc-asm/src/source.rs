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

use std::collections::BTreeMap;

use rucc_base::hash::{Map, Set};
use rucc_mir::CfiOp;
use rucc_object::{
    Array, Assembled, Binding, Extent, Group, Held, Keep, Name, Part, Reference, Reloc, Shape,
    Sort, Visibility,
};
use rucc_target::aarch64::{self, AAPCS64};
use rucc_target::x86;
use rucc_target::x86_64::{
    Mode, OldNops, SYSV, Width, gpr_named, nops, nops_before_2_42, nops_i386,
};
use rucc_target::{CallRegs, ObjectFormat};
use rucc_tuple::Arch;

/// What an instruction says about the place in it that names something, under a name that does not
/// collide with the [`Sort`] an ELF symbol has.
use crate::instruction::Sort as Reach;
use crate::lines::{self, Lines};
use crate::unwind::{Named, Prologue, Seh};

mod macros;

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

/// What a file of assembly for this machine says, as the sections and names an object file is
/// written from.
///
/// The machine is x86-64 or AArch64. The directives are the same on both, apart from a few that
/// gas spells differently on each, and so are the labels and the expressions. What differs is the
/// instructions, which on AArch64 are read by `rucc_target::aarch64::read` and encoded by the same
/// encoder the code generator's listings are checked against.
///
/// # Errors
///
/// [`Trouble`] for a directive this does not know, an instruction it has no bytes for, an operand
/// it cannot read, an expression that does not reduce to something a relocation can say, or a file
/// that is malformed. Every one of them carries the line it was on.
pub fn read(text: &str, arch: Arch) -> Result<Assembled, Trouble> {
    read_as(text, arch, ObjectFormat::Elf)
}

/// The same, for a file written for the object format given.
///
/// Only Mach-O reads differently. A section there is a segment and a section, `__TEXT,__text`,
/// and the file uses a handful of directives gas has no use for elsewhere, `.zerofill` and
/// `.private_extern` among them. ELF and COFF share the names this reads, and the COFF writer
/// makes its own out of them.
///
/// # Errors
///
/// As [`read`].
pub fn read_as(text: &str, arch: Arch, format: ObjectFormat) -> Result<Assembled, Trouble> {
    read_with(text, arch, format, Flags::default())
}

/// What a command line said to the assembler, which is the part of `-Wa,` that changes how a file
/// is read.
///
/// Most of what a build hands gas that way either describes what this assembler does anyway or is
/// refused by the driver, so this is short. `spec/04-driver-and-cli.md` section 4.9 has the list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Flags {
    /// `--fatal-warnings`, which makes a warning stop the file.
    pub fatal_warnings: bool,
    /// `--noexecstack`, which gives an ELF object the empty `.note.GNU-stack` that says its stack
    /// is not executable when the file did not write one itself. Without it gas writes none, and
    /// neither does this.
    pub noexecstack: bool,
    /// `-mrelax-relocations=no`, which asks for the relocation of a slot of the global offset table
    /// that the linker may not rewrite, `R_X86_64_GOTPCREL` and `R_386_GOT32`, in every instruction.
    pub keep_slots: bool,
    /// A gas before 2.42, which `-fgnu-as-version=` claims, and which pads after data the way it
    /// pads after an instruction, with no one byte `nop` in front. The private
    /// `Reader::after_data` says why.
    pub before_2_42: bool,
}

/// The same, with what the command line said to the assembler.
///
/// # Errors
///
/// As [`read`], and a `.warning` directive under [`Flags::fatal_warnings`].
pub fn read_with(
    text: &str,
    arch: Arch,
    format: ObjectFormat,
    flags: Flags,
) -> Result<Assembled, Trouble> {
    // Every branch starts out in its two byte form and the file is read again with the ones that
    // did not reach written long, until none is left over. A branch made long never goes back, so
    // each pass has more long ones than the last and there are only so many branches, which is how
    // gas does it and why the two come out the same size.
    //
    // A count worked out from labels further down is the other thing that sends the file round
    // again, with each label where the last pass put it, until the places stop moving. That has no
    // such guarantee, so it is given a number of passes and a file that is still moving after them
    // is refused rather than read for ever.
    // i386 COFF has `@` in names, `_Sleep@4` and `@f@8`, where everything below reads an `@` as
    // the start of a suffix. So the names are read with the `@` spelled some other way and given
    // it back at the end. The one suffix there is `@SECREL32`, which no decoration looks like.
    let decorated = arch == Arch::X86 && format == ObjectFormat::Coff && text.contains('@');
    let spelled;
    let text = if decorated {
        spelled = at_signs_out(text);
        spelled.as_str()
    } else {
        text
    };
    let mut long = Set::default();
    let mut guesses = Map::default();
    let mut moving = 0;
    let mut passes = 0;
    // Whether the passes are still on gas's first round. See [`Reader::first_round`].
    let mut first_round = true;
    let mut within = Map::default();
    loop {
        passes += 1;
        let aarch64 = arch == Arch::Aarch64;
        let i386 = arch == Arch::X86;
        let macho = format == ObjectFormat::MachO;
        let coff = format == ObjectFormat::Coff;
        let mut reader = Reader {
            long: long.clone(),
            guesses,
            first_round: passes > 1 && first_round,
            within: std::mem::take(&mut within),
            aarch64,
            i386,
            macho,
            coff,
            fatal_warnings: flags.fatal_warnings,
            noexecstack: flags.noexecstack,
            before_2_42: flags.before_2_42,
            ..Reader::default()
        };
        reader.run(text)?;
        match reader.finish()? {
            Ok(done) if decorated => return Ok(kept(at_signs_back(done), flags)),
            Ok(done) => return Ok(kept(done, flags)),
            Err(again) => {
                // The round is over once it grows nothing more, and at once when nothing in it
                // came out any different from the rounds after.
                if passes > 1 && (!again.first_round || again.grow.is_empty()) {
                    first_round = false;
                }
                if again.grow.is_empty() {
                    moving += 1;
                    if moving > MOST_PASSES {
                        let why = format!(
                            "the size of this depends on where labels end up, and after \
                             {MOST_PASSES} passes they still move"
                        );
                        return Err(Trouble { line: again.line, why });
                    }
                }
                long.extend(again.grow);
                guesses = again.places;
                within = again.within;
            }
        }
    }
}

/// A file read under [`Flags::keep_slots`], with every slot of the global offset table read through
/// a relocation the linker leaves alone.
fn kept(mut done: Assembled, flags: Flags) -> Assembled {
    if flags.keep_slots {
        for reloc in done.parts.iter_mut().flat_map(|part| part.relocs.iter_mut()) {
            reloc.kind = reloc.kind.kept();
        }
    }
    done
}

/// What an `@` in a name on i386 COFF is read as, which is a run of characters a name may hold and
/// none that a compiler or a person writes in one.
const AT_SIGN: &str = "__rucc_at__";

/// The file with every `@` outside a string spelled [`AT_SIGN`].
///
/// Only a string keeps its own, since an `@` there is a byte of data rather than a part of a
/// name, and a comment is left alone as well because nothing reads it. So does the `@` of
/// `_x@SECREL32(%eax)`, which is how far a thread-local variable is into `.tls` and is a suffix
/// here as everywhere else. A decoration is digits after the `@`, or a name for `fastcall`'s
/// leading one, and never that word on its own.
fn at_signs_out(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut quoted = false;
    let mut escaped = false;
    let mut comment = false;
    for (at, ch) in text.char_indices() {
        match ch {
            '\n' => {
                comment = false;
                quoted = false;
            }
            _ if comment => {}
            '\\' if quoted => {
                escaped = !escaped;
                out.push(ch);
                continue;
            }
            '"' if !escaped => quoted = !quoted,
            '#' if !quoted => comment = true,
            '@' if !quoted && !secrel(&text[at + 1..]) => {
                out.push_str(AT_SIGN);
                continue;
            }
            _ => {}
        }
        escaped = false;
        out.push(ch);
    }
    out
}

/// Whether what follows an `@` is the `SECREL32` suffix and nothing more of a name.
fn secrel(after: &str) -> bool {
    after.strip_prefix("SECREL32").is_some_and(|rest| {
        !rest.starts_with(|ch: char| {
            ch.is_ascii_alphanumeric() || matches!(ch, '_' | '$' | '.' | '@')
        })
    })
}

/// A file read with [`at_signs_out`], with its names given their `@` back.
fn at_signs_back(mut done: Assembled) -> Assembled {
    let back = |name: &mut String| {
        if name.contains(AT_SIGN) {
            *name = name.replace(AT_SIGN, "@");
        }
    };
    for part in &mut done.parts {
        back(&mut part.name);
        for reloc in &mut part.relocs {
            back(&mut reloc.symbol);
        }
        if let Some(group) = &mut part.group {
            back(&mut group.symbol);
        }
    }
    for name in &mut done.names {
        back(&mut name.name);
    }
    done
}

/// How many times the file is laid out again because a count depended on where a label ended up,
/// on top of the passes that make branches long.
///
/// gas settles the kernel's alternatives in two or three and so does this. A file that has not
/// settled after this many is one whose sizes chase each other, and gas gives up on those too.
const MOST_PASSES: usize = 64;

/// The most bytes a count worked out from guessed places may come to.
///
/// A count that feeds on itself, like `.fill (1f - 0b) * 2` between the two labels, doubles on
/// every pass, and it would run out of memory long before it ran out of passes. A real count that
/// is still a guess is never near this, so one that is has to be one of those.
const MOST_GUESSED: u64 = 1 << 24;

/// Why a pass over the file was not the answer, and what the next one needs to know.
#[derive(Debug)]
struct Again {
    /// The branches written short that do not reach.
    grow: Vec<usize>,
    /// Where every name ended up, which the next pass guesses from. See [`Reader::guesses`].
    places: Map<String, Held>,
    /// The first line that guessed, for a message about a file that never settles.
    line: usize,
    /// How far into its piece every label ended up. See [`Reader::first_round`].
    within: Map<String, u64>,
    /// Whether a count came out different for being on gas's first round.
    first_round: bool,
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

/// How many bytes a LEB128 number that names a label further on is given. See [`Reader::leb`].
const LEB_ROOM: u8 = 4;

/// The name of the global offset table, which gas turns into a distance to the table wherever it
/// is written on i386.
const TABLE: &str = "_GLOBAL_OFFSET_TABLE_";

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
    /// Which field of an AArch64 instruction these bytes are, where the answer goes into some bits
    /// of the word rather than into bytes of its own.
    field: Option<aarch64::Fixup>,
    /// Whether these bytes are a LEB128 number, and a signed one, which is written in all of them
    /// however few it needs. See [`Reader::leb`].
    leb: Option<bool>,
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
    /// What `.cfi_personality` said: the routine the unwinder calls for this frame, and how the
    /// pointer to it is written.
    personality: Option<(u8, String)>,
    /// What `.cfi_lsda` said: where the call site table the routine reads is, the same way.
    lsda: Option<(u8, String)>,
    /// Whether `.cfi_startproc` said `simple` and whether `.cfi_signal_frame` was written.
    begins: crate::unwind::Begins,
}

/// One function's prologue, as `.seh_` directives said it.
///
/// The same codes the object writer turns a compiled function's rows into, since the directives
/// are those codes one to a line, so a function read from text and the same function compiled
/// straight to an object get the same record. See [`crate::unwind::Seh`].
#[derive(Debug)]
struct Described {
    part: usize,
    start: u64,
    len: u64,
    /// The entry the row points at, made where `.seh_proc` was written, which is where gas counts
    /// the codes from as well.
    sym: usize,
    /// What `.seh_proc` called the function, for a message about it.
    name: String,
    prologue: Prologue,
    /// Whether `.seh_endprologue` has been read, after which there is nothing left to describe.
    ended: bool,
    /// The same function's codes on AArch64, where the prologue is not all there is: every
    /// epilogue is described as well. See [`crate::xdata`].
    arm: crate::xdata::Proc,
    /// The epilogue being read, between `.seh_startepilogue` and `.seh_endepilogue`.
    epilogue: Option<crate::xdata::Epilogue>,
}

/// The file, as it is being read.
#[derive(Debug, Default)]
struct Reader {
    parts: Vec<Part>,
    /// Which index each section name is at, so that a second `.text` continues the first one.
    named: Map<String, usize>,
    /// The section being written to.
    here: usize,
    /// What `.pushsection` stacked up, each with what `.previous` went back to then.
    stack: Vec<(usize, Option<usize>)>,
    /// What `.previous` goes back to.
    before: Option<usize>,
    /// The parts that are a numbered subsection of another, with the part that is the section
    /// itself and the number. See [`Reader::subsection`].
    subs: Map<usize, (usize, u64)>,
    /// The ELF sections a `.section` or a `.pushsection` named, which are written even when
    /// nothing went into them, as gas writes them. A linker script may keep one and take its
    /// address.
    declared: Set<usize>,
    syms: Vec<Sym>,
    known: Map<String, usize>,
    /// How many times each numbered local label has been written so far, which is what `1b` counts
    /// back from and what `1f` counts forward from.
    counts: Map<String, usize>,
    /// Which sections have a name pointing into them, so that an empty one that something is
    /// defined in survives and an empty one nothing mentions does not.
    labelled: Set<usize>,
    /// The piece of its section each label went in, numbered by [`crate::lines::Lines::cuts`]. Two
    /// labels in one piece are a fixed distance apart as soon as both are down. See
    /// [`Reader::fixed_now`].
    pieces: Map<usize, usize>,
    /// The sections whose last line was data rather than an instruction, which gas pads in front
    /// of with a one byte no-op before the long ones. See [`Reader::align`].
    after_data: Set<usize>,
    fixups: Vec<Fixup>,
    /// `.set` and `.equ`, as the symbol they name and the expression they were given.
    sets: Vec<(usize, Sum, usize)>,
    /// Which of `sets` each set entry is, so that a conditional can look through a name to what it
    /// was set to while the file is still being read. See [`macros`].
    setting: Map<usize, usize>,
    /// The names whose current setting is a plain number, by the name the file writes, with that
    /// number. gas puts the number in wherever such a name is used, so `.long type + (n << 8)` is
    /// arithmetic on numbers when `n` was set to one, which is what the kernel's exception table
    /// macros count on.
    values: Map<String, i64>,
    /// `.size`, the same way.
    sizes: Vec<(usize, Sum, usize)>,
    /// The names set to exactly another name, with that name's entry. gas gives one the other's
    /// type and size, so `.set alias, real` on a function is a second function and not a bare label.
    copies: Vec<(usize, String)>,
    /// `.symver name, name2@VERS`, as the name, the versioned name and the line. Worked out at the
    /// end, once the name has a place, by [`Reader::versions`].
    symvers: Vec<(String, String, usize)>,
    /// The names [`Reader::versions`] renamed, with the name each now has, which the relocations
    /// written against the old name are given at the end.
    renamed: Map<String, String>,
    /// Which entry a name that has been set means from here on. A file may set one name as many
    /// times as it likes, and each use means the value it had where the use was written, so a
    /// second setting is a second entry and this says which one is current.
    current: Map<String, String>,
    /// The names set to a register, `X0 = %xmm4` or `.set KEY, %rdi`, with the register. gas lets
    /// an instruction name the register that way, and the kernel's crypto code names nearly every
    /// register it uses so, setting the same name again as it rotates them round a loop.
    registers: Map<String, String>,
    /// The constants `ldr x0, =value` put in a literal pool on AArch64 that no `.ltorg` has
    /// written yet, each with the part it goes at the end of, the label the load reads, and its
    /// width.
    literals: Vec<(usize, String, String, u8)>,
    /// How many labels for a literal pool have been made, so the next one is new.
    literal_labels: usize,
    /// The numbered entries a relocation names, which are kept in the symbol table so that the
    /// relocation has something to point at. That is a numbered local label or a set name reached
    /// from another section, and is rare.
    relocated: Set<usize>,
    /// The names `.local` was said of, which a `.comm` after it makes room for here rather than
    /// asking the linker, the way `.lcomm` does. Every name is local until something says
    /// otherwise, so the binding alone cannot tell these apart.
    said_local: Set<usize>,
    /// The local commons given room in each part, by name and alignment, in the order the file
    /// wrote them. See [`Reader::join_subsections`].
    pooled: Map<usize, Vec<(usize, u64)>>,
    /// The function whose frame rules are being read, between `.cfi_startproc` and `.cfi_endproc`.
    frame: Option<Frame>,
    /// Every function that has had its frame rules read, in the order the file wrote them.
    frames: Vec<Frame>,
    /// The function whose prologue is being read, between `.seh_proc` and `.seh_endproc`.
    described: Option<Described>,
    /// Every function whose prologue has been read, in the order the file wrote them.
    prologues: Vec<Described>,
    /// The name a COFF `.def` is about, until its `.endef`.
    def: Option<usize>,
    /// Whether `.cfi_sections` left the unwind table out, which a file does when it wants the rules
    /// for a debugger only.
    no_unwind: bool,
    /// Whether `.cfi_sections` asked for the rules in `.debug_frame`, the copy a debugger reads and
    /// the loader never maps, which the kernel's boot code asks for in place of the unwind table.
    debugger: bool,
    /// What the file said it was called. Kept apart from the rest because it is not a name anything
    /// refers to, and a file whose own name is also the name of something in it would otherwise be
    /// one symbol where it should be two.
    files: Vec<String>,
    /// The branches an earlier pass found out of reach of two bytes, which this one writes long.
    long: Set<usize>,
    /// How many branches with a two byte form have been read so far.
    branches: usize,
    /// Every alignment in the file, in the order it was written.
    aligns: Vec<Aligned>,
    /// Where every name was when the pass before this one ended, which is the place a label
    /// further down the file is taken to have when an expression wants a number out of it now.
    ///
    /// That is what `.skip -((144f - 143f) > 0) * (144f - 143f)` needs, and the kernel writes one
    /// behind every alternative: how much padding goes here depends on how long some instructions
    /// in another section came out, which is not known until the whole file has been laid out, and
    /// the padding moves everything after it. gas makes such a `.skip` a piece of variable size and
    /// lays the file out again until nothing moves, and so does this, one whole pass at a time.
    guesses: Map<String, Held>,
    /// Whether this pass is on gas's first round, which reads a place in a section further down
    /// the list of sections as how far it is into its piece.
    ///
    /// gas lays out one section after another, and then all of them again for as long as anything
    /// moved. On the first round a section it has not got to yet has every piece of it at zero, so
    /// a label there is worth only how far it is into its piece. A count that is a distance between
    /// two such labels comes out right when they are in one piece and wrong when a jump that may
    /// grow is between them. The kernel's `.skip` for an alternative whose replacement has a jump
    /// in it is one, and it is padded as nothing on that round. The jumps that round grows stay
    /// grown, so the file has to go through it too.
    first_round: bool,
    /// How far into its piece each label was on the last pass, for [`Reader::first_round`].
    within: Map<String, u64>,
    /// Whether [`Reader::first_round`] made any count come out different.
    first_round_counted: std::cell::Cell<bool>,
    /// Whether a count is being worked out, which is the only thing [`Reader::first_round`] is for.
    sizing: bool,
    /// Every place this pass took from [`Reader::guesses`], with the line that took it. A pass is
    /// the answer only when every one of them turns out to be where it was guessed to be.
    guessed: Vec<(String, Held, usize)>,
    /// What would be wrong with this pass if its guesses held, which is a `.org` that went
    /// backwards on the strength of one. Said only once the guesses are known to be right.
    doubts: Vec<Trouble>,
    /// Whether the file is for AArch64 rather than x86-64.
    aarch64: bool,
    /// Whether the file is for i386, whose instructions are x86-64's read in thirty two bit mode
    /// and whose position independent code reaches things from the global offset table.
    i386: bool,
    /// The mode `.code32` or `.code64` last put instructions in, which is otherwise the file's
    /// own. The kernel's la57toggle.S switches to thirty two bits for the code it copies below
    /// four gigabytes, in the middle of an x86-64 file.
    code: Option<Mode>,
    /// Whether `.code16` or `.code16gcc` is the mode instead, which is thirty two bit code with the
    /// defaults turned round (see [`crate::sixteen`]), and if so whether it was the gcc one.
    sixteen: Option<bool>,
    /// Whether the file is for Mach-O, where a section is named by its segment as well.
    macho: bool,
    /// Whether the file is for COFF, whose `.section` flags are letters of their own.
    coff: bool,
    /// Whether the file said `.subsections_via_symbols`, which tells the linker it may take the
    /// file apart at every name.
    subsections: bool,
    /// Whether a warning stops the file, which is `-Wa,--fatal-warnings`.
    fatal_warnings: bool,
    /// [`Flags::before_2_42`].
    before_2_42: bool,
    /// Where each run of padding in code went under [`Flags::before_2_42`], as a piece, how far
    /// into it and how long, for the end of the file to write once the mode it ends in is known.
    old_padding: Vec<(usize, u64, usize)>,
    /// See [`Flags::noexecstack`].
    noexecstack: bool,
    /// The macros defined so far and the conditionals and repetitions that are open.
    macros: macros::Macros,
    /// What `.file` with a number and `.loc` said, for the line table. See [`crate::lines`].
    lines: Lines,
    line: usize,
}

impl Reader {
    /// Read the whole file.
    fn run(&mut self, text: &str) -> Result<(), Trouble> {
        // Before anything else, so that a file which never names a section still has one and a
        // stray directive has somewhere to go. gas starts in `.text` and so does this.
        if self.macho {
            self.apple_section("__TEXT", "__text", None, &[])?;
        } else {
            self.section(".text", Shape::of(".text"));
        }
        // gas makes `.data` and `.bss` as well before it reads a line, and an ELF object it writes
        // has all three whether the file put anything in them or not. Read back into `.text`
        // after, which is where a file that names no section starts.
        if self.elf() {
            for name in [".data", ".bss"] {
                self.section(name, Shape::of(name));
            }
            self.go(0);
        }
        let mut commenting = false;
        for (index, raw) in text.lines().enumerate() {
            self.line = index + 1;
            let line = self.strip(raw, &mut commenting)?;
            self.feed(&line)?;
        }
        if commenting {
            return Err(self.bad("a block comment was opened and never closed"));
        }
        self.finish_macros()?;
        // A pool no `.ltorg` wrote goes at the end of its section, as gas puts it.
        while let Some(&(part, ..)) = self.literals.first() {
            self.go(part);
            self.literal_pool()?;
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
            // On AArch64 a `#` in front of a number is part of the number, so only one that starts
            // the line is a comment, which is still enough for the line markers.
            let marker = ch == '#' && (!self.aarch64 || out.trim().is_empty());
            if rest.starts_with("//") || marker {
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
            let Some((name, length)) = labelled(text) else { break };
            self.label(&name)?;
            text = &text[length..];
        }
        let text = text.trim();
        if text.is_empty() {
            return Ok(());
        }
        // A directive's name ends at a bracket as well, since the arm64 kernel writes `.inst(x)`
        // with no blank so that it reads as one argument of a macro.
        let cut = if text.starts_with('.') {
            text.find(|ch: char| ch.is_whitespace() || ch == '(')
        } else {
            text.find(char::is_whitespace)
        };
        let (word, rest) = match cut {
            Some(cut) => (&text[..cut], text[cut..].trim()),
            None => (text, ""),
        };
        if let Some((name, what)) = assigned(text) {
            // Setting the place itself is `.org` in gas, and makes no symbol called `.`.
            if name == "." {
                return self.directive("org", what);
            }
            return self.assign(name, what);
        }
        if let Some(directive) = word.strip_prefix('.') {
            return self.directive(directive, rest);
        }
        if self.aarch64 {
            // `name .req register` gives a register a second name, which is how the arm64 kernel
            // has `lr` for `x30` and `wx0` for `w0` in `assembler.h`. Only the operands are
            // renamed, so an alias cannot take the place of a mnemonic.
            if let Some(register) =
                rest.strip_prefix(".req").filter(|after| after.starts_with(char::is_whitespace))
            {
                let register = register.trim();
                let register = self
                    .registers
                    .get(register)
                    .cloned()
                    .unwrap_or_else(|| register.to_ascii_lowercase());
                self.registers.insert(word.to_owned(), register);
                return Ok(());
            }
            let renamed = self.renamed(rest);
            if let Some(text) = self.wide_part(word, &renamed) {
                return self.a64(&text);
            }
            if let Some(text) = self.literal(word, &renamed) {
                return self.a64(&text);
            }
            if let Some(text) = self.label_sum(word, &renamed) {
                return self.a64(&text);
            }
            if self.registers.is_empty() && !rest.contains('#') && !rest.contains(['(', ' ']) {
                return self.a64(text);
            }
            let rest = self.bare_sums(&self.renamed(rest));
            let rest = self.worked_out(&rest);
            return self.a64(&format!("{word} {rest}"));
        }
        // A prefix written on the same line as the instruction it goes in front of, which is how a
        // kernel writes `lock` and how it writes `cs` in front of a call it wants a byte longer.
        // Taken off one at a time, so that `xacquire lock incl (%rax)` is two of them. gas reads a
        // mnemonic whatever its case, and the x86 selftests write `SYSENTER`.
        let (mut word, mut rest) = (word.to_ascii_lowercase(), rest);
        let mut prefixes = Vec::new();
        loop {
            if let Some((joined, after)) = repeated(&word, rest) {
                (word, rest) = (joined, after);
                break;
            }
            let Some(byte) = prefix(&word).filter(|_| !rest.is_empty()) else { break };
            prefixes.push(byte);
            let (next, after) = match rest.find(char::is_whitespace) {
                Some(cut) => (&rest[..cut], rest[cut..].trim()),
                None => (rest, ""),
            };
            (word, rest) = (next.to_ascii_lowercase(), after);
        }
        let rest = self.unaliased(rest);
        self.instruction(&word, &rest, &prefixes)
    }

    /// The operands with every name that was set to a register written as the register, and every
    /// name set to a plain number written as the number.
    ///
    /// The number matters because the operand is read apart from the rest of the file, and
    /// `8*t+frame_W(%rsp)` inside a `.rept` is arithmetic on two such names that would otherwise be
    /// read as two names added together. gas puts the numbers in the same way.
    ///
    /// A name only counts where it stands on its own: not after `%`, which is a register already,
    /// not after `\`, which is a macro argument nothing replaced, and not in the middle of a
    /// longer name or a number.
    fn unaliased(&self, rest: &str) -> String {
        self.replaced(rest, true)
    }

    /// The operands of an AArch64 instruction with every name `.req` gave a register written as
    /// the register. Numbers are left as they are, since [`Self::worked_out`] puts them in where
    /// they are immediates and a name anywhere else is a symbol.
    fn renamed(&self, rest: &str) -> String {
        self.replaced(rest, false)
    }

    /// The operands of an AArch64 instruction with every immediate that is arithmetic, or a name
    /// set to a number, written as the number it comes to.
    ///
    /// The AArch64 reader takes a number after `#` and nothing else, and the kernel's assembly is
    /// full of more than that once the preprocessor has been over it: `#(1 << 3)` for a flag,
    /// `#((0x40) | (0x80))` for two, and the `bti` macro it has for an assembler that does not
    /// know the instruction, which sets `.L__bti_targets_c` to 34 and writes `hint #.L__bti_targets_c`. An immediate
    /// runs to the comma or the bracket that ends it. One that names a label, or `:lo12:` and the
    /// like, is left for the reader, which says what it makes of it, unless the labels in it cancel:
    /// the trampoline vectors in the kernel's entry.S prefetch `[x30, #(1b - \vector_start)]`, the
    /// distance of one vector from the start of the table.
    fn worked_out(&mut self, rest: &str) -> String {
        let mut pieces = rest.split('#');
        let mut out = String::with_capacity(rest.len());
        out.push_str(pieces.next().unwrap_or(""));
        for piece in pieces {
            out.push('#');
            let mut depth = 0i32;
            let end = piece
                .char_indices()
                .find(|&(_, c)| {
                    match c {
                        '(' => depth += 1,
                        ')' => depth -= 1,
                        _ => {}
                    }
                    depth == 0 && matches!(c, ',' | ']')
                })
                .map_or(piece.len(), |(at, _)| at);
            let (immediate, after) = piece.split_at(end);
            let text = immediate.trim();
            let plain = text.is_empty()
                || text.starts_with(':')
                || text.parse::<f64>().is_ok()
                || text
                    .strip_prefix('-')
                    .unwrap_or(text)
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric());
            let number = (!plain || self.values.contains_key(text))
                .then(|| {
                    Parser {
                        text,
                        at: 0,
                        here: (0, 0),
                        values: Some(&self.values),
                        reader: None,
                        guessed: None,
                    }
                    .whole()
                    .ok()?
                    .flat()
                })
                .flatten();
            let number = match number {
                None if !plain => self.expression(text).ok().and_then(|sum| self.absolute(&sum)),
                number => number,
            };
            match number {
                Some(number) => out.push_str(&number.to_string()),
                None => out.push_str(immediate),
            }
            out.push_str(after);
        }
        out
    }

    /// The operands of an AArch64 instruction with every immediate written without its `#` that is
    /// arithmetic worked out and written with one.
    ///
    /// GNU as takes an immediate with no `#` in front of it, and the kernel writes some that way:
    /// `subs count, count, 128 + 16` in `memcpy.S`, and `cmp x0, ((0b0001))` once the preprocessor
    /// has been over a constant handed to a macro in `el2_setup.h`. An operand inside brackets or
    /// braces is an address or a list and is left alone, and so is one that is not all numbers.
    fn bare_sums(&self, rest: &str) -> String {
        let mut out = String::with_capacity(rest.len());
        let mut depth = 0i32;
        let mut start = 0;
        let mut pieces = Vec::new();
        for (at, c) in rest.char_indices() {
            match c {
                '[' | '{' | '(' => depth += 1,
                ']' | '}' | ')' => depth -= 1,
                ',' if depth == 0 => {
                    pieces.push(&rest[start..at]);
                    start = at + 1;
                }
                _ => {}
            }
        }
        pieces.push(&rest[start..]);
        for (index, piece) in pieces.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            let text = piece.trim();
            let sum = text.starts_with(|c: char| c == '(' || c == '~' || c.is_ascii_digit())
                && text.contains(['(', '+', '-', '*', '/', '<', '>', '|', '&', '^', '~', ' '])
                && !text.contains(['[', '{', ':', '#']);
            let number = sum
                .then(|| {
                    Parser {
                        text,
                        at: 0,
                        here: (0, 0),
                        values: Some(&self.values),
                        reader: None,
                        guessed: None,
                    }
                    .whole()
                    .ok()?
                    .flat()
                })
                .flatten();
            // A relocation specifier reads its sum with no spaces in it: `:lo12:table + 1`.
            let specified = text.trim_start_matches('#').starts_with(':');
            match number {
                Some(number) => out.push_str(&format!(" #{number}")),
                None if specified => {
                    out.push(' ');
                    out.extend(text.chars().filter(|c| !c.is_whitespace()));
                }
                None => out.push_str(piece),
            }
        }
        out
    }

    /// A load of a constant, `ldr x0, =value`, as a load of a word in a literal pool or a move.
    ///
    /// gas writes a number a `mov` can hold as that `mov`, and puts anything else, a symbol say,
    /// in a pool that `.ltorg` writes out, or the end of the section when nothing does. The arm64
    /// kernel's `head.S` loads the address of `__primary_switched` this way.
    fn literal(&mut self, word: &str, rest: &str) -> Option<String> {
        if !word.eq_ignore_ascii_case("ldr") {
            return None;
        }
        let (register, value) = rest.split_once(',')?;
        let value = value.trim().strip_prefix('=')?.trim();
        let register = register.trim();
        let width = match register.as_bytes().first()? {
            b'x' | b'X' => 8,
            b'w' | b'W' => 4,
            _ => return None,
        };
        let number = Parser {
            text: value,
            at: 0,
            here: (0, 0),
            values: Some(&self.values),
            reader: None,
            guessed: None,
        }
        .whole()
        .ok()
        .and_then(|sum| sum.flat());
        if let Some(number) = number {
            let number = if width == 4 { i64::from(number as u32) } else { number };
            let moved = format!("mov {register}, #{number}");
            let line = aarch64::read(&moved).ok()?;
            if aarch64::encode(&line.mnemonic, &line.values).is_ok() {
                return Some(moved);
            }
        }
        let label = format!(".Lrucc_literal{}", self.literal_labels);
        self.literal_labels += 1;
        self.literals.push((self.here, label.clone(), value.to_owned(), width));
        Some(format!("ldr {register}, {label}"))
    }

    /// The literal pool of the part being written to, after the code that loads from it.
    fn literal_pool(&mut self) -> Result<(), Trouble> {
        let here = self.here;
        let (pool, rest) =
            std::mem::take(&mut self.literals).into_iter().partition(|(part, ..)| *part == here);
        self.literals = rest;
        for (_, label, value, width) in pool {
            self.align("balign", &[width.to_string()])?;
            self.label(&label)?;
            self.data(&[value], width)?;
        }
        Ok(())
    }

    /// A wide move of sixteen bits out of a number, `movz x0, :abs_g3:0x1234`, as the plain move
    /// it is, `movz x0, #0x1234 >> 48 & 0xffff, lsl #48` with the number worked out.
    ///
    /// The arm64 kernel's `mov_q` writes a constant into a register with these, and the reader only
    /// takes the operators that leave a symbol for a relocation. `:abs_gN:` and `:abs_gN_nc:` are
    /// bits `16N` to `16N + 15` of the number, the first checking that nothing is above them.
    /// `:abs_gN_s:` is the signed version, which makes a `movz` of a negative number a `movn` of
    /// its complement, as GNU as does.
    ///
    /// The number may be a name set to labels, as the kernel's `tramp_alias` in entry.S sets
    /// `.Lalias` to `TRAMP_VALIAS + tramp_exit - .entry.tramp.text`, a label further down less the
    /// start of its section. Such a number is the one the last pass put the labels at, and is only
    /// checked to fit when nothing in it was guessed, since a guess the next pass corrects can be
    /// anything. Anything that is not a number here, a symbol say, is left for the reader.
    fn wide_part(&mut self, word: &str, rest: &str) -> Option<String> {
        let word = word.to_ascii_lowercase();
        if !matches!(word.as_str(), "movz" | "movk" | "movn") {
            return None;
        }
        let (register, operand) = rest.split_once(',')?;
        let operand = operand.trim();
        let operand = operand.strip_prefix('#').unwrap_or(operand).trim_start();
        let (operator, text) = operand.strip_prefix(':')?.split_once(':')?;
        let operator = operator.to_ascii_lowercase();
        let group = operator.strip_prefix("abs_g")?;
        let (group, kind) = group.split_at(1);
        let group: u32 = group.parse().ok().filter(|&group| group < 4)?;
        if !matches!(kind, "" | "_nc" | "_s") || (kind == "_s" && group == 3) {
            return None;
        }
        let flat = Parser {
            text: text.trim(),
            at: 0,
            here: (0, 0),
            values: Some(&self.values),
            reader: None,
            guessed: None,
        }
        .whole()
        .ok()
        .and_then(|sum| sum.flat());
        let guessed = self.guessed.len();
        let value = match flat {
            Some(value) => value,
            None => {
                let sum = self.expression(text).ok()?;
                self.absolute(&sum)?
            }
        };
        let sure = self.guessed.len() == guessed;
        let shift = 16 * group;
        let fits = |value: i64| !sure || shift + 16 >= 64 || value >> (shift + 16) == 0;
        let (word, part) = match kind {
            "_s" if word == "movz" && value < 0 => {
                if !fits(!value) {
                    return None;
                }
                ("movn", !value >> shift & 0xffff)
            }
            "_s" if !fits(value) => return None,
            "" if !fits(value) => return None,
            _ => (word.as_str(), value >> shift & 0xffff),
        };
        Some(format!("{word} {register}, #{part}, lsl #{shift}"))
    }

    /// The label of a branch or of `adr` with a sum added to it that comes to a number, written as
    /// the label and that number.
    ///
    /// The reader takes a label and a number after it. The KVM vectors in hyp-entry.S branch to
    /// `__kvm_hyp_vector + (1b - 0b + KVM_VECTOR_PREAMBLE)`, which is the same place in the other
    /// table, and the distance between two labels of this section is a number here.
    fn label_sum(&mut self, word: &str, rest: &str) -> Option<String> {
        const CONDS: [&str; 18] = [
            "eq", "ne", "cs", "hs", "cc", "lo", "mi", "pl", "vs", "vc", "hi", "ls", "ge", "lt",
            "gt", "le", "al", "nv",
        ];
        let word = word.to_ascii_lowercase();
        let cond = word.strip_prefix('b').map(|cond| cond.strip_prefix('.').unwrap_or(cond));
        let labels = ["b", "bl", "cbz", "cbnz", "tbz", "tbnz", "adr", "adrp"];
        if !labels.contains(&word.as_str()) && !cond.is_some_and(|cond| CONDS.contains(&cond)) {
            return None;
        }
        let mut depth = 0;
        let mut cut = 0;
        for (at, c) in rest.char_indices() {
            match c {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                ',' if depth == 0 => cut = at + 1,
                _ => {}
            }
        }
        let (before, last) = (&rest[..cut], rest[cut..].trim());
        let end = last.find(|c: char| !(c.is_ascii_alphanumeric() || "_.$".contains(c)))?;
        let (name, sum) = (&last[..end], last[end..].trim_start());
        let symbolic = name.starts_with(|c: char| c.is_ascii_alphabetic() || "_.$".contains(c));
        if !symbolic || self.values.contains_key(name) {
            return None;
        }
        let negative = match sum.as_bytes().first() {
            Some(b'+') => false,
            Some(b'-') => true,
            _ => return None,
        };
        let added = self.expression(&sum[1..]).ok()?;
        let added = self.absolute(&added)?;
        let added = if negative { added.wrapping_neg() } else { added };
        let sign = if added < 0 { '-' } else { '+' };
        Some(format!("{word} {before}{name}{sign}{}", added.unsigned_abs()))
    }

    /// The operands with the names this file gave registers, and with the numbers it set when
    /// `numbers` says so, put in place of the names.
    fn replaced(&self, rest: &str, numbers: bool) -> String {
        if self.registers.is_empty() && (self.values.is_empty() || !numbers) {
            return rest.to_owned();
        }
        let bytes = rest.as_bytes();
        let mut out = String::with_capacity(rest.len());
        let mut at = 0;
        while at < bytes.len() {
            let byte = bytes[at];
            // `$` may be part of a name, but in front of one in an operand it says an immediate.
            if !carries_on(byte) || byte == b'$' {
                let ch = rest[at..].chars().next().unwrap_or(' ');
                out.push(ch);
                at += ch.len_utf8();
                continue;
            }
            let end =
                at + rest[at..].find(|ch: char| !carries_on(ch as u8)).unwrap_or(rest.len() - at);
            let word = &rest[at..end];
            let after = at.checked_sub(1).map(|before| bytes[before]);
            let alone = starts(byte) && !matches!(after, Some(b'%' | b'\\'));
            if !alone {
                out.push_str(word);
            } else if let Some(register) = self.registers.get(word) {
                out.push_str(register);
            } else if let Some((register, lanes)) = word
                .split_once('.')
                .and_then(|(name, lanes)| Some((self.registers.get(name)?, lanes)))
            {
                // A vector register named with its lanes, `k0.4s` after `k0 .req v0`, which is how
                // the arm64 kernel's SHA-1 names its round constants.
                out.push_str(register);
                out.push('.');
                out.push_str(lanes);
            } else if let Some(&value) = self.values.get(word).filter(|_| numbers) {
                match value < 0 {
                    true => out.push_str(&format!("({value})")),
                    false => out.push_str(&value.to_string()),
                }
            } else {
                out.push_str(word);
            }
            at = end;
        }
        out
    }

    /// The number an immediate comes to as the line is read, when every place in it is a label this
    /// pass has already put down in one piece of this section, and the places cancel.
    ///
    /// gas works such a difference out as it reads the line, since nothing between the two labels
    /// can change size, and then picks the short form of the instruction for a number that fits a
    /// byte. So `subl $(1b - startup_32), %ebp` in the kernel's compressed boot code is three
    /// bytes and not six. A label further down, or one behind an alignment or a jump that may grow,
    /// is a distance gas only knows at the end, and gets the long form, as before.
    fn fixed_now(&mut self, arg: &str) -> Option<i64> {
        let text = arg.trim().strip_prefix('$')?;
        if text.contains('@') || constant(text).is_some() {
            return None;
        }
        let here = (self.here, self.at() as i64);
        let sum = self.expression_at(text, here).ok()?;
        let (mut value, mut net, mut piece) = (sum.constant, 0, None);
        for term in &sum.terms {
            let (offset, at) = match &term.what {
                What::Here { at, .. } => (*at, self.lines.cuts(self.here)),
                What::Symbol(name) => {
                    let sym = *self.known.get(name)?;
                    match self.syms[sym].at {
                        Held::Absolute(number) => {
                            value = value.wrapping_add(term.coeff.wrapping_mul(number as i64));
                            continue;
                        }
                        Held::In { part, offset } if part == self.here => {
                            (offset as i64, *self.pieces.get(&sym)?)
                        }
                        _ => return None,
                    }
                }
            };
            if *piece.get_or_insert(at) != at {
                return None;
            }
            value = value.wrapping_add(term.coeff.wrapping_mul(offset));
            net += term.coeff;
        }
        (piece.is_some() && net == 0).then_some(value)
    }

    /// The section a part is, which is itself unless it is a numbered subsection of another.
    fn section_of(&self, part: usize) -> usize {
        self.subs.get(&part).map_or(part, |&(parent, _)| parent)
    }

    /// The mode the next instruction is read in: what `.code32` or `.code64` last said, and
    /// otherwise the one the file's machine runs in.
    fn mode(&self) -> Mode {
        self.code.unwrap_or(if self.i386 { Mode::Bits32 } else { Mode::Bits64 })
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
    ///
    /// `prefixes` are the bytes of any prefix the line wrote in front of the mnemonic, which go in
    /// among the ones the instruction already has in the order gas puts them.
    fn instruction(&mut self, word: &str, rest: &str, prefixes: &[u8]) -> Result<(), Trouble> {
        self.after_data.remove(&self.section_of(self.here));
        let mut args = if rest.is_empty() { Vec::new() } else { split(rest, ',') };
        for arg in &mut args {
            if let Some(value) = self.fixed_now(arg) {
                *arg = format!("${value}");
            }
        }
        let mode = self.mode();
        let mut written = match self.sixteen {
            Some(gcc) => crate::sixteen::one_in16(word, &args, gcc),
            None => crate::instruction::one_in(word, &args, mode),
        }
        .map_err(|why| self.bad(&why))?;
        if self.before_2_42 && self.sixteen.is_none() && mode == Mode::Bits64 {
            crate::instruction::widened_before_2_42(word, &args, &mut written);
        }
        // `jmp .+10` has been given its short form already, where the distance is known.
        let mut branch = None;
        let short = crate::instruction::short(&written).filter(|_| written.holes[0].name != ".");
        // gas works a jump to a global name in this section out itself, and on i386 leaves one
        // that asked for `@PLT` to the linker, since the name may be taken from another object.
        let stub =
            self.i386 && written.holes.first().is_some_and(|hole| hole.sort == Reach::Branch);
        let relaxed = short.is_some();
        let jump = relaxed && !stub;
        if let Some(short) = short {
            if !self.long.contains(&self.branches) {
                branch = Some(self.branches);
                written = short;
            }
            self.branches += 1;
        }
        for &byte in prefixes {
            prefixed(&mut written, byte).map_err(|why| self.bad(&why))?;
        }
        let part = self.here;
        let at = self.at();
        self.row()?;
        self.put(&written.bytes)?;
        let end = at + written.bytes.len() as u64;
        // A jump that may grow is a piece of its own in gas, which matters to the line table.
        if relaxed {
            self.lines.variable(part, at + 1, end);
        }
        let slot = if self.i386 {
            slot_i386(&written.bytes)
        } else {
            crate::bytes::slot(&written.bytes, mode)
        };
        for hole in written.holes {
            // `.` in an instruction is where the instruction starts, which is what gas means by it
            // and what `mov .-4(%rip), %eax` counts back from.
            let here = (part, at as i64);
            let sum = if matches!(
                hole.sort,
                Reach::Value
                    | Reach::Extended
                    | Reach::Offset
                    | Reach::Slot
                    | Reach::Tls(_)
                    | Reach::Section
            ) {
                // The number itself, with nothing taken off for where the instruction ends. A name
                // in an address carries what is added to it apart, and a number carries nothing.
                let mut sum = self.expression_at(&hole.name, here)?;
                sum.constant += hole.addend;
                // The global offset table's own name in an i386 instruction is the distance from
                // these bytes to the table, which gas counts from the start of the instruction. So
                // `addl $_GLOBAL_OFFSET_TABLE_, %ebx` has the two bytes in front of the number
                // added, and the `+[.-.L1]` gcc used to write moves the start back to the label.
                if self.i386 && sum.terms.iter().any(|term| term.what.is(TABLE)) {
                    sum.constant += hole.at as i64;
                }
                sum
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
                field: None,
                leb: None,
                line: self.line,
            });
        }
        Ok(())
    }

    /// One AArch64 instruction, as the four bytes of it.
    ///
    /// The same division as for x86-64: the word comes back with zeros where a name goes, and the
    /// name is left as a fixup for the end of the file. What is different is what the fixup is
    /// counted from. A branch, `adr` and a literal load count from where the instruction starts,
    /// so the sum is the name minus that and one in this section is worked out here. The rest name
    /// a page or the low bits of an address, which only the linker knows, so the sum is the name.
    fn a64(&mut self, text: &str) -> Result<(), Trouble> {
        let line = aarch64::read(text).map_err(|why| self.bad(&why.to_string()))?;
        let encoded = aarch64::encode(&line.mnemonic, &line.values)
            .map_err(|why| self.bad(&why.to_string()))?;
        let (part, at) = (self.here, self.at());
        self.put(&encoded.word.to_le_bytes())?;
        let (Some(field), Some(name)) = (encoded.fixup, line.symbol) else {
            return Ok(());
        };
        // `.` on its own is where this instruction starts, which is how a loop inside one opcode
        // of the listing branches back to its own top.
        let what = if name == "." {
            What::Here { part, at: at as i64 }
        } else {
            let name = self.named(&name)?;
            self.sym(&name);
            What::Symbol(name)
        };
        let mut terms = vec![Term { coeff: 1, what }];
        if relative(field) {
            terms.push(Term { coeff: -1, what: What::Here { part, at: at as i64 } });
        }
        let jump = matches!(
            field,
            aarch64::Fixup::Jump26 | aarch64::Fixup::CondBr19 | aarch64::Fixup::TestBr14
        );
        self.fixups.push(Fixup {
            part,
            at,
            width: 4,
            sum: Sum { constant: line.addend, terms },
            reach: Reach::Near,
            slot: Reference::Data,
            branch: None,
            jump,
            field: Some(field),
            leb: None,
            line: self.line,
        });
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
        self.pieces.insert(sym, self.lines.cuts(part));
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
        let what = what.trim();
        let register = match what.strip_prefix('%') {
            Some(_) => Some(what.to_owned()),
            None => self.registers.get(what).cloned(),
        };
        if let Some(register) = register.filter(|_| !self.aarch64) {
            self.registers.insert(name.to_owned(), register);
            return Ok(());
        }
        self.registers.remove(name);
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
        match sum.flat() {
            Some(value) => self.values.insert(name.to_owned(), value),
            None => self.values.remove(name),
        };
        if let [Term { coeff: 1, what: What::Symbol(target) }] = sum.terms.as_slice() {
            if sum.constant == 0 {
                let target = self.current.get(target).unwrap_or(target).clone();
                self.copies.push((sym, target));
            }
        }
        self.setting.insert(sym, self.sets.len());
        self.sets.push((sym, sum, self.line));
        Ok(())
    }

    /// A frame rule, which says what an unwinder standing at this instruction should believe.
    ///
    /// What the rules say is the same [`CfiOp`] the compiler's own functions are described with,
    /// and the table is written from them by the same code, so a function read from text and the
    /// same function compiled straight to an object unwind the same way. The directives that say
    /// something this table has no row for are passed over as they were before any of this was
    /// read, which leaves those functions described as well as a C function needs. The personality
    /// routine and the call site table are kept, since a function with a `cleanup` under
    /// `-fexceptions` is not described without them. See [`crate::unwind::Named`].
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
                    cfa: i32::try_from(self.conv().return_address).expect("a word"),
                    remembered: Vec::new(),
                    personality: None,
                    lsda: None,
                    begins: crate::unwind::Begins {
                        simple: args.first().is_some_and(|arg| arg.trim() == "simple"),
                        signal: false,
                    },
                };
                self.frame = Some(frame);
                return Ok(());
            }
            "cfi_signal_frame" => {
                let Some(frame) = &mut self.frame else {
                    return Err(
                        self.bad("a frame rule outside '.cfi_startproc' and '.cfi_endproc'")
                    );
                };
                frame.begins.signal = true;
                return Ok(());
            }
            "cfi_sections" => {
                self.no_unwind = !args.iter().any(|arg| arg.trim() == ".eh_frame");
                self.debugger = args.iter().any(|arg| arg.trim() == ".debug_frame");
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
            | "cfi_restore_state"
            | "cfi_personality"
            | "cfi_lsda"
            | "cfi_negate_ra_state" => {}
            // The B key needs a header of its own that says so, which this does not write, and
            // leaving the directive out would have the unwinder check a signature with the wrong key.
            "cfi_b_key_frame" => {
                return Err(self.bad(
                    "'.cfi_b_key_frame', a return address signed with the B key, which this \
                     assembler writes no unwind header for",
                ));
            }
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
                if frame.lsda.is_some() && frame.personality.is_none() {
                    return Err(bad("a '.cfi_lsda' with no '.cfi_personality' to read it"));
                }
                frame.len = at - frame.start;
                self.frames.push(frame);
                return Ok(());
            }
            // Passed over on Mach-O, whose table is written without either for the reason
            // `crate::unwind::table` gives, and which spells them in encodings of its own.
            "cfi_personality" | "cfi_lsda" if self.macho => {
                self.frame = Some(frame);
                return Ok(());
            }
            "cfi_personality" | "cfi_lsda" => {
                let said = self.handler(word, args);
                let said = match said {
                    Ok(said) => said,
                    Err(trouble) => {
                        self.frame = Some(frame);
                        return Err(trouble);
                    }
                };
                if word == "cfi_personality" {
                    frame.personality = said;
                } else {
                    frame.lsda = said;
                }
                self.frame = Some(frame);
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
                if offset % self.conv().word as i32 != 0 {
                    return Err(bad(
                        "a register saved somewhere that is not a whole number of slots from the \
                         end of the frame, which is the only place this writes a rule for",
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
            "cfi_negate_ra_state" => CfiOp::NegateRaState,
            _ => unreachable!("every other word returned above"),
        };
        frame.rows.push(((at - frame.start) as usize, op));
        self.frame = Some(frame);
        Ok(())
    }

    /// One of the directives that describe a prologue for the table Windows reads.
    ///
    /// Each is one code of the record, said after the instruction it is about, so what is kept is
    /// the code and where that instruction ended. `.seh_endprologue` says where the prologue ends
    /// and `.seh_endproc` where the function does. A handler for the frame is refused, since the
    /// record this writes has no room for one.
    fn seh(&mut self, word: &str, args: &[String]) -> Result<(), Trouble> {
        if word == "seh_proc" {
            if self.described.is_some() {
                return Err(self.bad("a '.seh_proc' inside another one"));
            }
            let sym = self.sym(&format!("\u{1}seh{}", self.prologues.len()));
            let (part, start) = (self.here, self.at());
            self.syms[sym].at = Held::In { part, offset: start };
            let name = args.first().map_or("", |arg| arg.trim()).to_owned();
            let prologue = Prologue::default();
            let ended = false;
            self.described = Some(Described {
                part,
                start,
                len: 0,
                sym,
                name,
                prologue,
                ended,
                arm: crate::xdata::Proc::default(),
                epilogue: None,
            });
            return Ok(());
        }
        let (here, at) = (self.here, self.at());
        let Some(described) = &self.described else {
            return Err(self.bad(&format!("a '.{word}' outside '.seh_proc' and '.seh_endproc'")));
        };
        if described.part != here {
            return Err(self.bad("a '.seh_' directive in another section from its function"));
        }
        if self.aarch64 {
            return self.seh_arm(word, args);
        }
        let offset = (at - described.start) as usize;
        let ended = described.ended;
        let code = match word {
            "seh_endprologue" | "seh_endproc" => {
                let described = self.described.as_mut().expect("checked above");
                if word == "seh_endprologue" {
                    described.prologue.end = offset;
                    described.ended = true;
                    return Ok(());
                }
                // A file that never said where the prologue ends is taken to end it with its last
                // code, which is where the object writer ends one.
                if !described.ended {
                    described.prologue.end = described.prologue.codes.last().map_or(0, |c| c.0);
                }
                described.len = at - described.start;
                let described = self.described.take().expect("checked above");
                self.prologues.push(described);
                return Ok(());
            }
            "seh_handler" | "seh_handlerdata" => {
                return Err(self.bad(&format!(
                    "a '.{word}', which names an exception handler for the frame, and the unwind \
                     record this writes has no handler in it"
                )));
            }
            _ if ended => {
                return Err(self.bad(&format!("a '.{word}' after '.seh_endprologue'")));
            }
            "seh_pushreg" => Seh::Push(self.seh_reg(args.first().map_or("", String::as_str))?),
            "seh_stackalloc" => Seh::Alloc(self.number(args.first().map_or("", String::as_str))?),
            "seh_setframe" => {
                let [reg, offset] = self.two(args, ".seh_setframe")?;
                Seh::Frame { reg: self.seh_reg(&reg)?, offset: self.number(&offset)? }
            }
            "seh_savereg" => {
                let [reg, above] = self.two(args, ".seh_savereg")?;
                Seh::Save { reg: self.seh_reg(&reg)?, vector: false, above: self.number(&above)? }
            }
            "seh_savexmm" => {
                let [reg, above] = self.two(args, ".seh_savexmm")?;
                let name = reg.trim().trim_start_matches('%');
                let number = name.strip_prefix("xmm").and_then(|n| n.parse::<u8>().ok());
                let Some(reg) = number.filter(|n| *n < 16) else {
                    return Err(self
                        .bad(&format!("'{}' is not a register '.seh_savexmm' saves", reg.trim())));
                };
                Seh::Save { reg, vector: true, above: self.number(&above)? }
            }
            _ => {
                let what = format!(
                    "'.{word}' is a directive this compiler does not know, so nothing was written \
                     for it"
                );
                return Err(self.bad(&what));
            }
        };
        let described = self.described.as_mut().expect("checked above");
        described.prologue.codes.push((offset, code));
        Ok(())
    }

    /// One of the directives that describe a function for the table Windows reads on AArch64.
    ///
    /// These are a code per instruction, for the prologue and for every epilogue. The prologue runs
    /// from `.seh_proc` to `.seh_endprologue` and an epilogue from `.seh_startepilogue` to
    /// `.seh_endepilogue`, and each has to be as many instructions long as it has codes, since an
    /// unwinder stopped inside one counts its way back by them. That is the check clang makes too,
    /// and it leaves where in the prologue each directive is written to the file.
    fn seh_arm(&mut self, word: &str, args: &[String]) -> Result<(), Trouble> {
        let at = self.at();
        let described = self.described.as_mut().expect("checked by the caller");
        let offset = (at - described.start) as usize;
        let counted = |what: &str, bytes: usize, codes: usize| {
            (bytes != 4 * codes).then(|| {
                format!(
                    "{what} {bytes} bytes long with {codes} unwind codes, and it needs one code \
                     for each instruction, if only '.seh_nop'"
                )
            })
        };
        match word {
            "seh_endprologue" => {
                if described.ended {
                    return Err(self.bad("a second '.seh_endprologue'"));
                }
                described.ended = true;
                if let Some(why) = counted("a prologue", offset, described.arm.prologue.len()) {
                    return Err(self.bad(&why));
                }
                return Ok(());
            }
            "seh_startepilogue" => {
                if described.epilogue.is_some() {
                    return Err(self.bad("a '.seh_startepilogue' inside another epilogue"));
                }
                described.ended = true;
                described.epilogue =
                    Some(crate::xdata::Epilogue { start: offset, codes: Vec::new() });
                return Ok(());
            }
            "seh_endepilogue" => {
                let Some(epilogue) = described.epilogue.take() else {
                    return Err(self.bad("a '.seh_endepilogue' with no '.seh_startepilogue'"));
                };
                let why = counted("an epilogue", offset - epilogue.start, epilogue.codes.len());
                described.arm.epilogues.push(epilogue);
                return match why {
                    Some(why) => Err(self.bad(&why)),
                    None => Ok(()),
                };
            }
            // A funclet is a piece of a function with a record of its own, which only a handler
            // needs and nothing here writes, so the end of one is the end of the function's code.
            "seh_endfunclet" => return Ok(()),
            "seh_endproc" => {
                if described.epilogue.is_some() {
                    return Err(self.bad("a '.seh_endproc' inside an epilogue"));
                }
                described.len = at - described.start;
                let described = self.described.take().expect("checked above");
                self.prologues.push(described);
                return Ok(());
            }
            "seh_handler" | "seh_handlerdata" => {
                return Err(self.bad(&format!(
                    "a '.{word}', which names an exception handler for the frame, and the unwind \
                     record this writes has no handler in it"
                )));
            }
            _ => {}
        }
        let code = match crate::xdata::Code::read(word, args) {
            Some(Ok(code)) => code,
            Some(Err(why)) => return Err(self.bad(&why)),
            None => {
                let what = format!(
                    "'.{word}' is a directive this compiler does not know, so nothing was written \
                     for it"
                );
                return Err(self.bad(&what));
            }
        };
        match &mut described.epilogue {
            Some(epilogue) => epilogue.codes.push(code),
            None if !described.ended => described.arm.prologue.push(code),
            None => {
                return Err(self.bad(&format!(
                    "a '.{word}' after '.seh_endprologue' and outside an epilogue"
                )));
            }
        }
        Ok(())
    }

    /// The machine's number for a general purpose register a `.seh_` directive names.
    fn seh_reg(&self, text: &str) -> Result<u8, Trouble> {
        let text = text.trim();
        gpr_named(text.strip_prefix('%').unwrap_or(text)).map(|(reg, _)| reg.number()).ok_or_else(
            || self.bad(&format!("'{text}' is not a register a '.seh_' directive names")),
        )
    }

    /// The encoding and the name `.cfi_personality` or `.cfi_lsda` gave, or nothing for the
    /// encoding that says there is none.
    ///
    /// The name gets an entry in the symbol table, since the unwind table reaches it through a
    /// relocation, and an encoding the table has no way to write is refused rather than written
    /// some other way. See [`crate::unwind::pointer_size`].
    fn handler(&mut self, word: &str, args: &[String]) -> Result<Option<(u8, String)>, Trouble> {
        let Some(first) = args.first() else {
            return Err(self.bad(&format!("a '.{word}' with no encoding")));
        };
        let encoding = self.number(first)?;
        let encoding = u8::try_from(encoding)
            .map_err(|_| self.bad(&format!("{encoding} is not the encoding of a pointer")))?;
        if encoding == crate::unwind::OMIT {
            return Ok(None);
        }
        let word_size = self.conv().word as u8;
        if crate::unwind::pointer_size(encoding, word_size, word == "cfi_personality").is_none() {
            return Err(self.bad(&format!(
                "a '.{word}' in encoding {encoding:#x}, which is not one the unwind table here \
                 can write"
            )));
        }
        let [_, name] = args else {
            return Err(self.bad(&format!("a '.{word}' wants an encoding and a name")));
        };
        let name = self.named(name.trim())?;
        let sym = self.sym(&name);
        self.relocated.insert(sym);
        Ok(Some((encoding, name)))
    }

    /// A distance in a frame rule, which is a number and not negative for the end of the frame.
    fn distance(&mut self, text: &str) -> Result<i32, Trouble> {
        let value = self.number(text)?;
        i32::try_from(value).map_err(|_| self.bad(&format!("{value} is not a distance in a frame")))
    }

    /// The calling convention whose state at a call every frame rule starts from, which is also
    /// where the width of a slot and of a pointer in the unwind table come from.
    fn conv(&self) -> &'static CallRegs {
        if self.aarch64 {
            &AAPCS64
        } else if self.i386 {
            &x86::SYSV
        } else {
            &SYSV
        }
    }

    /// The number DWARF gives a register a frame rule names, which a file may write either way.
    fn dwarf(&self, text: &str) -> Result<u16, Trouble> {
        let text = text.trim();
        if let Ok(number) = text.parse::<u16>() {
            return Ok(number);
        }
        if self.aarch64 {
            return aarch64_dwarf(text).ok_or_else(|| {
                self.bad(&format!("'{text}' is not a register a frame rule can name"))
            });
        }
        let name = text.strip_prefix('%').unwrap_or(text);
        // The flags and the six segment registers, which a signal frame says are in the
        // `sigcontext` it points into. gas's numbers, which are the psABI's for each machine.
        let other = match (name, self.i386) {
            ("eflags", true) => Some(9),
            ("rflags", false) => Some(49),
            _ => ["es", "cs", "ss", "ds", "fs", "gs"]
                .iter()
                .position(|&segment| segment == name)
                .map(|at| at as u16 + if self.i386 { 40 } else { 50 }),
        };
        if let Some(number) = other {
            return Ok(number);
        }
        // The eight thirty two bit registers and `eip`, in i386's own numbering, which is not the
        // order x86-64 gave the same registers.
        if self.i386 {
            if name == "eip" {
                return Ok(x86::DWARF_RETURN_ADDRESS);
            }
            return gpr_named(name)
                .filter(|&(reg, width)| width == Width::Long && reg.number() < 8)
                .and_then(|(reg, _)| x86::SYSV.dwarf(x86::GPR, reg))
                .ok_or_else(|| {
                    self.bad(&format!("'{text}' is not a register a frame rule can name"))
                });
        }
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
        if DATA.contains(&word) {
            self.after_data.insert(self.section_of(self.here));
        }
        match word {
            "text" | "data" | "bss" | "rodata" | "const" | "cstring" | "const_data"
                if self.macho =>
            {
                self.apple_plain(word, rest)?;
            }
            "text" | "data" | "bss" | "rodata" => {
                self.plain(word, rest)?;
            }
            "section" if self.macho => self.apple_section_directive(&args)?,
            "section" => self.section_directive(&args)?,
            "linkonce" if self.coff => self.linkonce(rest)?,
            "linkonce" if !self.macho => self.elf_linkonce(rest)?,
            "zerofill" if self.macho => self.zerofill(&args, false)?,
            "tbss" if self.macho => self.zerofill(&args, true)?,
            "subsections_via_symbols" if self.macho => self.subsections = true,
            "private_extern" if self.macho => self.sight(&args, Visibility::Hidden)?,
            "weak_definition" | "weak_reference" if self.macho => {
                self.bind(&args, Binding::Weak)?;
            }
            // The platform and the oldest version of it the file is for. The writer puts the
            // target's own in the file, which is where the compiler's listing got these from.
            "build_version"
            | "macosx_version_min"
            | "ios_version_min"
            | "tvos_version_min"
            | "watchos_version_min"
            | "data_region"
            | "end_data_region"
                if self.macho => {}
            // A number after the name is a subsection, which `.section` does not take on ELF and
            // this one does.
            "pushsection" => {
                self.stack.push((self.here, self.before));
                let (was, before) = (self.here, self.before);
                let numbered = args.get(1).filter(|arg| !arg.trim().starts_with('"'));
                match numbered {
                    Some(number) => {
                        let number = self.subsection_number(number)?;
                        let mut args = args.clone();
                        args.remove(1);
                        self.section_directive(&args)?;
                        self.subsection(number);
                    }
                    None => self.section_directive(&args)?,
                }
                self.came_from(was, before);
            }
            "subsection" => {
                let (was, before) = (self.here, self.before);
                let number = self.subsection_number(rest)?;
                self.subsection(number);
                self.came_from(was, before);
            }
            "popsection" => {
                // gas puts `.previous` back to what it was at the push too, so a template that
                // pushes and pops inside `.section .fixup` leaves `.previous` going back to the
                // section before `.fixup`, and not to the one the template pushed.
                let Some((back, before)) = self.stack.pop() else {
                    return Err(self.bad(".popsection with nothing pushed"));
                };
                (self.here, self.before) = (back, before);
            }
            "previous" => {
                let Some(back) = self.before else {
                    return Err(self.bad(".previous with no section before this one"));
                };
                self.go(back);
            }

            "ltorg" | "pool" if self.aarch64 => self.literal_pool()?,
            // An instruction written as its number, which is how the arm64 kernel writes one the
            // assembler may not know.
            "inst" if self.aarch64 => self.data(&args, 4)?,
            "byte" => self.data(&args, 1)?,
            // `.word` is two bytes on x86-64 and four on AArch64, where a word is an instruction.
            "word" if self.aarch64 => self.data(&args, 4)?,
            "short" | "word" | "hword" | "value" | "2byte" => self.data(&args, 2)?,
            "long" | "int" | "4byte" => self.data(&args, 4)?,
            "quad" | "8byte" | "xword" | "dword" => self.data(&args, 8)?,
            "octa" => self.octa(&args)?,
            "uleb128" => self.leb(&args, false)?,
            "sleb128" => self.leb(&args, true)?,

            "ascii" => self.text_bytes(&args, false)?,
            "asciz" | "string" => self.text_bytes(&args, true)?,

            "incbin" => self.incbin(&args)?,
            "reloc" => self.reloc(&args)?,

            "space" | "skip" | "zero" => {
                if args.is_empty() || args.len() > 2 {
                    return Err(self.bad(&format!(".{word} wants a size and an optional fill")));
                }
                let guessed = self.guessed.len();
                let size = self.size(&args[0])?;
                let fill = match args.get(1) {
                    Some(arg) => self.byte(arg)?,
                    None => 0,
                };
                let at = self.at();
                self.pad(size, fill)?;
                // A size that rests on a guess is one gas could not work out yet either, and it
                // makes such a `.skip` a piece of its own.
                if self.guessed.len() > guessed {
                    self.lines.variable(self.here, at, at + size);
                }
            }
            "fill" => {
                // The middle operand is the width of one item and the last is its value, and the
                // default width is one byte, which is why `.fill 8` is eight zero bytes and not
                // eight of anything else.
                if args.is_empty() || args.len() > 3 {
                    return Err(self.bad(".fill wants a count and an optional width and value"));
                }
                let count = self.size(&args[0])?;
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
                let guessed = self.guessed.len();
                let to = self.origin(first)?;
                let fill = match args.get(1) {
                    Some(arg) => self.byte(arg)?,
                    None => 0,
                };
                let at = self.at();
                if to < at {
                    let what = format!(".org back to {to} from {at}, which would overwrite bytes");
                    // On the strength of a guess it may only be the guess that is wrong, so it is
                    // said only if the guess turns out right. See [`Reader::guesses`].
                    if self.guessed.len() > guessed {
                        self.doubts.push(self.bad(&what));
                    } else {
                        return Err(self.bad(&what));
                    }
                } else {
                    self.pad(to - at, fill)?;
                }
                self.lines.variable(self.here, at, to.max(at));
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

            // What COFF says about a name in its own symbol table, between `.def` and `.endef`: a
            // storage class, which `.globl` has already said as much of as a link depends on, and
            // a type, where thirty two is a function.
            "def" if self.coff => {
                let name = args.first().map_or("", |arg| arg.trim());
                self.def = Some(self.sym(name));
            }
            "scl" if self.coff && self.def.is_some() => {}
            "type" if self.coff && self.def.is_some() => {
                let what = self.number(rest)?;
                if what == 32 {
                    let sym = self.def.expect("checked above");
                    self.syms[sym].sort = Sort::Func;
                }
            }
            "endef" if self.coff => self.def = None,
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
            "symver" if self.elf() => {
                let [name, versioned] = self.two(&args, ".symver")?;
                let versioned = versioned.trim();
                if !versioned.contains('@') {
                    let what = format!("'{versioned}' has no '@' to say which version it is");
                    return Err(self.bad(&what));
                }
                let name = self.current.get(name.trim()).cloned().unwrap_or(name);
                self.symvers.push((name.trim().to_owned(), versioned.to_owned(), self.line));
            }

            // Two directives under one name. `.file "foo.c"` says what this was assembled from and
            // becomes a symbol, and `.file 1 "foo.c"` is a line table entry which says the same
            // thing to a debugger and does not. The number in front is the whole difference.
            "file" => {
                let what = args.first().map_or("", |arg| arg.trim());
                if what.starts_with('"') {
                    self.files.push(unquoted(what));
                } else if !what.is_empty() {
                    self.numbered_file(rest)?;
                }
            }
            "loc" => self.loc(rest)?,

            // Said for a debugger or a reader and holding nothing a link depends on. Passed over
            // rather than refused, because a file that carries them is otherwise readable and
            // refusing would turn a note into a failure.
            "ident" if self.elf() => self.ident(&args)?,
            "ident" | "loc_mark_labels" | "version" | "arch" | "arch_extension" | "cpu"
            | "att_syntax" | "intel_syntax" => {}
            "code32" | "code64" if !self.aarch64 => {
                self.code = Some(if word == "code32" { Mode::Bits32 } else { Mode::Bits64 });
                self.sixteen = None;
            }
            "code16" | "code16gcc" if !self.aarch64 => {
                self.code = Some(Mode::Bits32);
                self.sixteen = Some(word == "code16gcc");
            }
            // gas takes every name it does not know to be defined elsewhere, so `.extern` says
            // nothing it would not have assumed anyway.
            "extern" => {}
            // The end of a name `.req` gave a register.
            "unreq" if self.aarch64 => {
                self.registers.remove(rest.trim());
            }
            // The one warning this assembler has. gas prints it and carries on, and nothing here has
            // anywhere to print to, so it is passed over like the notes above. Under
            // `--fatal-warnings` gas stops on it instead, and so does this, since a build that
            // asked for warnings to be fatal and got an object has been told the file was clean.
            "warning" if self.fatal_warnings => {
                let what = unquoted(args.first().map_or("", |arg| arg.trim()));
                return Err(self.bad(&format!(
                    "the file warns, and --fatal-warnings makes that an error: {what}"
                )));
            }
            "warning" => {}
            _ if word.starts_with("cfi_") => self.cfi(word, &args)?,
            _ if word.starts_with("seh_") && self.coff => self.seh(word, &args)?,

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
        // A number after one of these is a subsection. See [`Reader::subsection`].
        let number = self.subsection_number(rest)?;
        let (was, before) = (self.here, self.before);
        let name = format!(".{word}");
        let shape = Shape::of(&name);
        self.section(&name, shape);
        self.subsection(number);
        self.came_from(was, before);
        Ok(())
    }

    /// The number a `.subsection`, a `.text` or a `.pushsection` gave, or zero when it gave none.
    fn subsection_number(&mut self, text: &str) -> Result<u64, Trouble> {
        if text.trim().is_empty() {
            return Ok(0);
        }
        let number = self.number(text)?;
        u64::try_from(number).map_err(|_| {
            self.bad(&format!("{number} is not a subsection, which is never negative"))
        })
    }

    /// Go to that subsection of the section being written to.
    ///
    /// A subsection is a run of a section that gas lays out after the runs with lower numbers,
    /// whatever order the file wrote them in, so `.subsection 1` is how a template puts its slow
    /// path at the end of `.text` without naming a section of its own. Each one is a part of its
    /// own here while the file is read, since that is what keeps the bytes of each in the order
    /// they were written, and [`Reader::join_subsections`] puts them behind their section at the
    /// end. Zero is the section itself.
    fn subsection(&mut self, number: u64) {
        let parent = self.subs.get(&self.here).map_or(self.here, |&(parent, _)| parent);
        if number == 0 {
            self.go(parent);
            return;
        }
        // No section name starts with a zero byte, so this key is nothing a `.section` can say.
        let key = format!("\0{parent}\0{number}");
        if let Some(&at) = self.named.get(&key) {
            self.go(at);
            return;
        }
        let at = self.parts.len();
        let of = &self.parts[parent];
        self.parts.push(Part {
            name: of.name.clone(),
            bytes: Vec::new(),
            size: 0,
            align: 1,
            shape: of.shape,
            relocs: Vec::new(),
            group: of.group.clone(),
            link: of.link.clone(),
        });
        self.named.insert(key, at);
        self.subs.insert(at, (parent, number));
        self.go(at);
    }

    /// Say that `.previous` goes back to where the file was before a directive that went through
    /// a section on the way to a subsection of it, which is one move to gas and two here.
    fn came_from(&mut self, was: usize, before: Option<usize>) {
        self.before = if self.here == was { before } else { Some(was) };
    }

    /// The local commons of a subsection that holds nothing else, given room at the end of the
    /// section they belong to, as gas gives it them.
    ///
    /// gas lays out every subsection one after another and aligns each common where it lands, so
    /// one that only needs a byte goes straight after the last thing in `.bss`, and the next one
    /// is aligned from there. Moving the subsection whole, at the alignment of the most aligned
    /// thing in it, would leave a gap in front of the first. mpparse.c has four bytes of its own
    /// in `.bss` and then a common byte and a common word, which gas puts at 4 and 8, and a block
    /// moved whole put them at 8 and 16. Nothing is in such a subsection but room, so there are
    /// no bytes to move and nothing else in it counts from where it starts.
    fn pool(&mut self, parent: usize, sub: usize) -> bool {
        if self.parts[parent].shape.bits {
            return false;
        }
        let Some(pooled) = self.pooled.remove(&sub) else { return false };
        // Only when the commons are all there is, which is when laying them out again from zero
        // comes to what the subsection already is.
        let mut end = 0u64;
        for &(sym, align) in &pooled {
            end = end.next_multiple_of(align) + self.syms[sym].size;
        }
        if end != self.parts[sub].size {
            return false;
        }
        let mut end = self.parts[parent].size;
        for (sym, align) in pooled {
            let offset = end.next_multiple_of(align);
            self.syms[sym].at = Held::In { part: parent, offset };
            end = offset + self.syms[sym].size;
        }
        let align = self.parts[sub].align;
        let whole = &mut self.parts[parent];
        whole.size = end;
        whole.align = whole.align.max(align);
        self.parts[sub].size = 0;
        true
    }

    /// Every subsection behind the section it is part of, in the order of their numbers, and every
    /// place that pointed into one moved to where it went.
    ///
    /// Each one starts on the boundary it asked for, which is the largest alignment written inside
    /// it, so an alignment worked out while it was a part of its own is still right where it lands.
    /// gas may put one closer when the boundary happens to fall that way already, and the bytes that
    /// makes different are padding in either case. The padding is no-ops in code, since the one in
    /// front may fall through into it, and zeroes everywhere else.
    fn join_subsections(&mut self) {
        if self.subs.is_empty() {
            return;
        }
        let mut order: Vec<(usize, u64, usize)> =
            self.subs.iter().map(|(&sub, &(parent, number))| (parent, number, sub)).collect();
        order.sort_unstable();
        let mode = self.mode();
        let mut moved: Map<usize, (usize, u64)> = Map::default();
        for (parent, _, sub) in order {
            let taken = std::mem::take(&mut self.parts[sub].bytes);
            let relocs = std::mem::take(&mut self.parts[sub].relocs);
            let (size, align) = (self.parts[sub].size, self.parts[sub].align.max(1));
            if self.pool(parent, sub) {
                continue;
            }
            let exec = self.parts[parent].shape.exec;
            let whole = &mut self.parts[parent];
            let start = whole.size.next_multiple_of(align);
            if whole.shape.bits {
                let need = usize::try_from(start - whole.size).unwrap_or(0);
                if exec && self.aarch64 {
                    whole.bytes.resize(whole.bytes.len() + need % 4, 0);
                    for _ in 0..need / 4 {
                        whole.bytes.extend_from_slice(&A64_NOP.to_le_bytes());
                    }
                } else if exec && self.i386 {
                    nops_i386(mode, need, &mut whole.bytes);
                } else if exec {
                    nops(need, &mut whole.bytes);
                } else {
                    whole.bytes.resize(whole.bytes.len() + need, 0);
                }
                whole.bytes.extend(taken);
            }
            whole.size = start + size;
            whole.align = whole.align.max(align);
            let shift = usize::try_from(start).unwrap_or(usize::MAX);
            whole
                .relocs
                .extend(relocs.into_iter().map(|reloc| Reloc { at: reloc.at + shift, ..reloc }));
            self.parts[sub].size = 0;
            if self.labelled.contains(&sub) {
                self.labelled.insert(parent);
            }
            moved.insert(sub, (parent, start));
        }
        let place = |part: &mut usize, at: &mut u64| {
            if let Some(&(parent, start)) = moved.get(part) {
                *part = parent;
                *at += start;
            }
        };
        for sym in &mut self.syms {
            if let Held::In { part, offset } = &mut sym.at {
                place(part, offset);
            }
        }
        for fixup in &mut self.fixups {
            place(&mut fixup.part, &mut fixup.at);
        }
        for aligned in &mut self.aligns {
            place(&mut aligned.part, &mut aligned.at);
        }
        for frame in &mut self.frames {
            place(&mut frame.part, &mut frame.start);
        }
        for described in &mut self.prologues {
            place(&mut described.part, &mut described.start);
        }
        for row in self.lines.rows_mut() {
            place(&mut row.part, &mut row.at);
        }
        let sums = self.fixups.iter_mut().map(|fixup| &mut fixup.sum);
        let sums = sums.chain(self.sets.iter_mut().map(|(_, sum, _)| sum));
        for sum in sums.chain(self.sizes.iter_mut().map(|(_, sum, _)| sum)) {
            for term in &mut sum.terms {
                if let What::Here { part, at } = &mut term.what {
                    if let Some(&(parent, start)) = moved.get(part) {
                        *part = parent;
                        *at += start as i64;
                    }
                }
            }
        }
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
        if self.coff {
            return self.coff_section(&name, Shape::of(&name), args);
        }
        // No flags means the name decides, which is what makes `.section .text` the same section as
        // `.text` rather than an unallocated one that happens to share its name. A name gas has no
        // entry for gets no flags. Letters with no type keep the type the name gives, so
        // `.note.gnu.property` with `"a"` is still a note. See [`Shape::unflagged`].
        let named = Shape::unflagged(&name);
        let mut shape = named;
        let (mut merge, mut strings, mut grouped, mut linked) = (false, false, false, false);
        if let Some(flags) = args.get(1) {
            let letters = unquoted(flags.trim());
            shape = Shape {
                bits: named.bits,
                note: named.note,
                array: named.array,
                ..Shape::default()
            };
            for letter in letters.chars() {
                match letter {
                    'a' => shape.alloc = true,
                    'w' => shape.write = true,
                    'x' => shape.exec = true,
                    'T' => shape.thread = true,
                    'M' => merge = true,
                    'S' => strings = true,
                    'G' => grouped = true,
                    'R' => shape.retain = true,
                    // The section it goes with, which comes after the type and is read past below.
                    'o' => linked = true,
                    // Excluded from the link, and a section of large data. Taking either as an
                    // ordinary section of the same bytes is correct and merely larger.
                    'e' | 'd' => {}
                    _ => {
                        let what = format!("'{letter}' is not a section flag this compiler knows");
                        return Err(self.bad(&what));
                    }
                }
            }
            let implied = Shape::implied(&name);
            shape.alloc |= implied.alloc;
            shape.write |= implied.write;
            shape.exec |= implied.exec;
            shape.thread |= implied.thread;
        }
        if let Some(kind) = args.get(2) {
            let kind = kind.trim().trim_start_matches(['@', '%']);
            let kind = unquoted(kind);
            match kind.as_str() {
                "progbits" => {
                    shape.bits = true;
                    shape.note = false;
                }
                "nobits" => shape.bits = false,
                "init_array" => shape.array = Some(Array::Init),
                "fini_array" => shape.array = Some(Array::Fini),
                "preinit_array" => shape.array = Some(Array::Preinit),
                "note" => {
                    shape.bits = true;
                    shape.note = true;
                }
                _ => {
                    let what = format!("'{kind}' is not a section type this compiler writes");
                    return Err(self.bad(&what));
                }
            }
        }
        // How long an entry is follows the type, and a section with `M` and no length, or one this
        // cannot read, is taken as an ordinary one, which is correct and merely larger. Then the
        // section `o` goes with, and then the group `G` puts it in with the word that makes that a
        // COMDAT, in the order gas reads them.
        let mut next = 3;
        if merge {
            shape.merge = args.get(next).and_then(|entry| entry.trim().parse().ok()).unwrap_or(0);
            shape.strings = strings;
            next += 1;
        }
        let link = match linked.then(|| args.get(next)).flatten() {
            Some(symbol) => Some(unquoted(symbol.trim())),
            None if linked => return Err(self.bad("an 'o' section with no name it goes with")),
            None => None,
        };
        if linked {
            next += 1;
        }
        let group = match grouped.then(|| args.get(next)).flatten() {
            Some(symbol) => {
                let keep = match args.get(next + 1).map(|word| word.trim()) {
                    Some("comdat") => Keep::Any,
                    None => Keep::Together,
                    Some(word) => {
                        return Err(self.bad(&format!("'{word}' is not a kind of section group")));
                    }
                };
                Some(Group { symbol: unquoted(symbol.trim()), keep })
            }
            None if grouped => return Err(self.bad("a section group with no name")),
            None => None,
        };
        self.section_in(&name, shape, group, link);
        self.declared.insert(self.here);
        Ok(())
    }

    /// `.linkonce [selection]`, on ELF, which makes the section a COMDAT group about its own name,
    /// the one copy of it the linker keeps being any one. gas takes the words COFF has for which
    /// copy, and ELF has no way to say them.
    fn elf_linkonce(&mut self, rest: &str) -> Result<(), Trouble> {
        let word = rest.trim();
        if !word.is_empty() && Keep::of(word).is_none() {
            return Err(self.bad(&format!("'{word}' is not a COMDAT selection")));
        }
        let part = &mut self.parts[self.here];
        part.group = Some(Group { symbol: part.name.clone(), keep: Keep::Any });
        Ok(())
    }

    /// `.section name[, "flags"[, selection, symbol]]`, on COFF, where the letters are COFF's and
    /// the last two make the section a COMDAT. See [`Shape::coff`] for the letters.
    fn coff_section(&mut self, name: &str, named: Shape, args: &[String]) -> Result<(), Trouble> {
        let shape = match args.get(1) {
            Some(flags) => Shape::coff(&unquoted(flags.trim())).map_err(|letter| {
                self.bad(&format!("'{letter}' is not a COFF section flag this compiler knows"))
            })?,
            None => named,
        };
        let group = match (args.get(2), args.get(3)) {
            (None, _) => None,
            (Some(word), Some(symbol)) => {
                let word = word.trim();
                let Some(keep) = Keep::of(word) else {
                    return Err(self.bad(&format!("'{word}' is not a COMDAT selection")));
                };
                Some(Group { symbol: unquoted(symbol.trim()), keep })
            }
            (Some(_), None) => return Err(self.bad("a COMDAT section with no symbol")),
        };
        self.section_in(name, shape, group, None);
        Ok(())
    }

    /// `.linkonce [selection]`, on COFF, which is how gcc and gas make the section they are in a
    /// COMDAT. `discard` when nothing is named, as in gas. The symbol the group is about is not
    /// written, and is the first one the section defines, which [`Reader::finish`] fills in
    /// once every label is known. That is the symbol clang names in its own spelling of the same
    /// thing, `.section name,"dr",discard,symbol`, so the two spellings make the same object.
    fn linkonce(&mut self, rest: &str) -> Result<(), Trouble> {
        let word = match rest.trim() {
            "" => "discard",
            word => word,
        };
        let Some(keep) = Keep::of(word) else {
            return Err(self.bad(&format!("'{word}' is not a COMDAT selection")));
        };
        self.parts[self.here].group = Some(Group { symbol: String::new(), keep });
        Ok(())
    }

    /// `.text` and the other short names Apple's assembler has for a section, on Mach-O.
    fn apple_plain(&mut self, word: &str, rest: &str) -> Result<(), Trouble> {
        if !rest.trim().is_empty() && rest.trim() != "0" {
            let what =
                format!("'.{word} {}' is a subsection, which is not written yet", rest.trim());
            return Err(self.bad(&what));
        }
        let (segment, section) = match word {
            "text" => ("__TEXT", "__text"),
            "data" => ("__DATA", "__data"),
            "bss" => ("__DATA", "__bss"),
            "cstring" => ("__TEXT", "__cstring"),
            "const_data" => ("__DATA", "__const"),
            _ => ("__TEXT", "__const"),
        };
        self.apple_section(segment, section, None, &[])
    }

    /// `.section segment,section[,type[,attribute+attribute]]`, on Mach-O.
    fn apple_section_directive(&mut self, args: &[String]) -> Result<(), Trouble> {
        let [segment, section, ..] = args else {
            return Err(self.bad(".section on Mach-O wants a segment and a section"));
        };
        let kind = args.get(2).map(|kind| kind.trim()).filter(|kind| !kind.is_empty());
        let attributes = args.get(3).map_or(String::new(), |list| list.trim().to_owned());
        let attributes: Vec<&str> =
            attributes.split('+').map(str::trim).filter(|word| !word.is_empty()).collect();
        // A fifth operand is the size of one stub in a section of them, which only a linker's
        // own output has.
        if args.len() > 4 {
            return Err(self.bad("a Mach-O section with a stub size is not written"));
        }
        self.apple_section(segment.trim(), section.trim(), kind, &attributes)
    }

    /// Go to a Mach-O section, making it the first time.
    fn apple_section(
        &mut self,
        segment: &str,
        section: &str,
        kind: Option<&str>,
        attributes: &[&str],
    ) -> Result<(), Trouble> {
        let shape =
            Shape::mach(segment, section, kind, attributes).map_err(|why| self.bad(&why))?;
        self.section(&format!("{segment},{section}"), shape);
        Ok(())
    }

    /// `.zerofill segment,section,name,size,power` and `.tbss name,size,power`.
    ///
    /// Room for a name in a section of zeroes, made without going to that section, which is how
    /// Apple's assembler writes what gas would write as `.bss` and a label. `.zerofill` with only
    /// the first two operands makes the section and nothing in it.
    fn zerofill(&mut self, args: &[String], thread: bool) -> Result<(), Trouble> {
        let (segment, section, rest) = if thread {
            ("__DATA".to_owned(), "__thread_bss".to_owned(), args)
        } else {
            let [segment, section, rest @ ..] = args else {
                return Err(self.bad(".zerofill wants a segment and a section"));
            };
            (segment.trim().to_owned(), section.trim().to_owned(), rest)
        };
        // Made or found and not gone to, so neither where the file is writing nor what
        // `.previous` means changes.
        let (was, before) = (self.here, self.before);
        self.apple_section(&segment, &section, None, &[])?;
        let at = self.here;
        (self.here, self.before) = (was, before);
        let Some(name) = rest.first() else {
            return Ok(());
        };
        if !(2..=3).contains(&rest.len()) || self.parts[at].shape.bits {
            let what = format!("'{segment},{section}' is not a section this can make room in");
            return Err(self.bad(&what));
        }
        let size = self.number(&rest[1])?;
        let size = self.count(size)?;
        let align = match rest.get(2) {
            Some(arg) => {
                let power = self.number(arg)?;
                let power = self.count(power)?;
                if power > 15 {
                    return Err(self.bad(&format!("an alignment of 2^{power} is too large")));
                }
                1 << power
            }
            None => 1,
        };
        let sym = self.sym(name.trim());
        let part = &mut self.parts[at];
        part.align = part.align.max(align);
        part.size = part.size.next_multiple_of(align);
        let offset = part.size;
        part.size += size;
        self.labelled.insert(at);
        self.syms[sym].at = Held::In { part: at, offset };
        self.syms[sym].size = size;
        if self.syms[sym].sort == Sort::Untyped {
            self.syms[sym].sort = if thread { Sort::Thread } else { Sort::Object };
        }
        Ok(())
    }

    /// Go to a section, making it if this is the first time the file has named it.
    ///
    /// The flags are taken from the first mention. A second `.section .text,"ax"` after a plain
    /// `.text` says the same thing gas already worked out, and a file that really does contradict
    /// itself is one gas warns about and keeps the first answer for.
    fn section(&mut self, name: &str, shape: Shape) {
        self.section_in(name, shape, None, None);
    }

    /// The same, for a section that may be a COFF COMDAT. Two COMDATs of one name about two
    /// different symbols are two sections, which is the point of them: every function gets a
    /// `.text` of its own that the linker may drop.
    fn section_in(&mut self, name: &str, shape: Shape, group: Option<Group>, link: Option<String>) {
        let mut key = match &group {
            Some(group) => format!("{name}\0{}", group.symbol),
            None => name.to_owned(),
        };
        // One section for each name an `o` section goes with, as gas does, because each one is
        // kept or dropped with its own text and one section cannot go with two.
        if let Some(link) = &link {
            key = format!("{key}\0\0{link}");
        }
        if let Some(&at) = self.named.get(&key) {
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
            group,
            link,
        });
        self.named.insert(key, at);
        self.go(at);
    }

    /// A name nothing in the file defines that is the name of one of its sections, which gas takes
    /// to be the start of that section.
    ///
    /// The vDSO's exception table is written that way: each entry is `.long (from) - __ex_table`
    /// in the section `__ex_table`, the distance to the place from the start of the table, which is
    /// a distance from these bytes once the name is a place in the same section. gas puts no name
    /// for it in the table, so the name is given a `\u{1}` the way a numbered label is, which
    /// keeps it out, and a relocation that names it alone is written against the section.
    fn sections_by_name(&mut self) {
        if !self.elf() {
            return;
        }
        for sym in &mut self.syms {
            if matches!(sym.at, Held::Undefined) && !sym.numbered {
                if let Some(&part) = self.named.get(&sym.name) {
                    sym.at = Held::In { part, offset: 0 };
                    let unseen = format!("{}\u{1}start", sym.name);
                    self.renamed.insert(std::mem::replace(&mut sym.name, unseen.clone()), unseen);
                }
            }
        }
    }

    /// The symbol of each `.linkonce` group, which is the first name its section defines.
    fn leaders(&mut self) -> Result<(), Trouble> {
        for at in 0..self.parts.len() {
            if self.subs.contains_key(&at)
                || !self.parts[at].group.as_ref().is_some_and(|group| group.symbol.is_empty())
            {
                continue;
            }
            let first = self
                .syms
                .iter()
                .filter(|sym| !sym.numbered)
                .filter_map(|sym| match sym.at {
                    Held::In { part, offset } if part == at => Some((offset, &sym.name)),
                    _ => None,
                })
                .min_by_key(|(offset, _)| *offset);
            let Some((_, name)) = first else {
                let what = format!(
                    "'{}' is a .linkonce section that defines no symbol",
                    self.parts[at].name
                );
                return Err(self.bad(&what));
            };
            let name = name.clone();
            if let Some(group) = self.parts[at].group.as_mut() {
                group.symbol = name;
            }
        }
        Ok(())
    }

    /// Whether the object is ELF, which is neither of the other two formats a reader is told of.
    fn elf(&self) -> bool {
        !self.macho && !self.coff
    }

    /// Go to a section that exists, remembering where this came from for `.previous`.
    fn go(&mut self, at: usize) {
        if at != self.here {
            self.before = Some(self.here);
            self.here = at;
        }
    }

    /// `.ident`, which gas writes into `.comment` as a string with a zero byte in front of the
    /// first one, and which leaves the file in the section it was in, with the same one before it.
    fn ident(&mut self, args: &[String]) -> Result<(), Trouble> {
        let (here, before) = (self.here, self.before);
        let fresh = !self.named.contains_key(".comment");
        let shape = Shape { bits: true, merge: 1, strings: true, ..Shape::default() };
        self.section(".comment", shape);
        if fresh {
            self.put(&[0])?;
        }
        self.text_bytes(args, true)?;
        (self.here, self.before) = (here, before);
        Ok(())
    }

    /// `.byte`, `.long` and the rest, at the width each of them means.
    fn data(&mut self, args: &[String], width: u8) -> Result<(), Trouble> {
        if args.is_empty() {
            return Err(self.bad("a data directive with nothing after it"));
        }
        for arg in args {
            // How far a name is from the global offset table, or a slot of it, which is what gcc
            // fills a jump table with in i386 code that is position independent: `.long .L3@GOTOFF`.
            let suffix = crate::instruction::unsuffixed(arg).filter(|_| self.i386);
            let (sum, reach, slot) = match suffix {
                Some((rest, how @ ("GOTOFF" | "GOT"))) => {
                    if width != 4 {
                        return Err(self.bad(&format!(
                            "'@{how}' in {width} bytes, and the global offset table only reaches \
                             four"
                        )));
                    }
                    let sum = self.expression(&rest)?;
                    if how == "GOT" {
                        (sum, Reach::Slot, Reference::SlotKept)
                    } else {
                        (sum, Reach::Offset, Reference::GotOffset)
                    }
                }
                // Where a thread-local variable is, as gcc writes it into the debug information:
                // `.long x@dtpoff`. gas takes every thread-local suffix here and so does this.
                Some((rest, how)) if crate::instruction::threaded(how).is_some() => {
                    if width != 4 {
                        return Err(self.bad(&format!(
                            "'@{how}' in {width} bytes, and a thread-local offset on i386 is four"
                        )));
                    }
                    let tls = crate::instruction::threaded(how).expect("matched above");
                    (self.expression(&rest)?, Reach::Tls(tls), Reference::Got)
                }
                _ => (self.expression(arg)?, Reach::Near, Reference::Got),
            };
            let at = self.at();
            if let (Some(value), Reach::Near) = (sum.flat(), reach) {
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
                reach,
                slot,
                branch: None,
                jump: false,
                field: None,
                leb: None,
                line: self.line,
            });
        }
        Ok(())
    }

    /// `.reloc`, a relocation asked for by name at a place in this section that was written
    /// already, which is how the kernel's KVM build lists the addresses its hypervisor code holds:
    /// `.word 0` and then `.reloc 0, R_AARCH64_PREL32, __hyp_section_.text + 0x18`.
    ///
    /// Only the relocations that fill bytes of data with an address or a distance are taken, and
    /// they are written as the same expression would be in `.long` or `.quad` at that place, so the
    /// writer picks the same relocation the name asks for.
    fn reloc(&mut self, args: &[String]) -> Result<(), Trouble> {
        let [place, name, rest @ ..] = args else {
            return Err(self.bad(".reloc wants a place, a relocation and an expression"));
        };
        let name = name.trim();
        let (width, relative) = match name {
            "R_AARCH64_ABS64" | "R_X86_64_64" | "BFD_RELOC_64" => (8, false),
            "R_AARCH64_ABS32" | "R_X86_64_32" | "R_386_32" | "BFD_RELOC_32" => (4, false),
            "R_AARCH64_ABS16" | "R_X86_64_16" | "R_386_16" | "BFD_RELOC_16" => (2, false),
            "R_AARCH64_PREL64" | "R_X86_64_PC64" | "BFD_RELOC_64_PCREL" => (8, true),
            "R_AARCH64_PREL32" | "R_X86_64_PC32" | "R_386_PC32" | "BFD_RELOC_32_PCREL" => (4, true),
            "R_AARCH64_PREL16" | "R_X86_64_PC16" | "R_386_PC16" | "BFD_RELOC_16_PCREL" => (2, true),
            _ => {
                let what = format!("'.reloc' of '{name}', which is not a relocation of data");
                return Err(self.bad(&what));
            }
        };
        let at = self.origin(place)?;
        if at + width > self.at() {
            let what = format!(".reloc at {at}, past the bytes this section has so far");
            return Err(self.bad(&what));
        }
        let mut sum = match rest {
            [] => Sum::default(),
            [one] => self.expression(one)?,
            _ => return Err(self.bad(".reloc wants one expression after the relocation")),
        };
        let part = self.here;
        if relative {
            sum.terms.push(Term { coeff: -1, what: What::Here { part, at: at as i64 } });
        }
        self.fixups.push(Fixup {
            part,
            at,
            width: width as u8,
            sum,
            reach: Reach::Near,
            slot: Reference::Got,
            branch: None,
            jump: false,
            field: None,
            leb: None,
            line: self.line,
        });
        Ok(())
    }

    /// `.uleb128` and `.sleb128`, a number in as many bytes as it needs, seven bits to a byte.
    ///
    /// What a call site table is written in, where each number is how far one label is from
    /// another. A number that can be worked out where it is written is written in as few bytes as
    /// it needs, which is what gas writes. That is every label behind it in this pass, since a pass
    /// lays out each section as it goes and one whose branches do not reach is read again from the
    /// top. One that cannot, which is a label further on, is worked out from where the last pass
    /// put that label and written in as few bytes as that needs, and the file is read again until
    /// the places stop moving, as for any other count that looks ahead. gas does the same, so the
    /// length of a call site table or of a DWARF expression the kernel's vDSO writes by hand comes
    /// out in one byte where it fits in one.
    fn leb(&mut self, args: &[String], signed: bool) -> Result<(), Trouble> {
        if args.is_empty() {
            return Err(self.bad("a data directive with nothing after it"));
        }
        for arg in args {
            let sum = self.expression(arg)?;
            let known = self.reduce(&sum).ok().filter(|residue| residue.left.is_empty());
            if let Some(residue) = known {
                let mut bytes = Vec::new();
                if signed {
                    crate::unwind::sleb(&mut bytes, residue.constant);
                } else {
                    let value = u64::try_from(residue.constant).map_err(|_| {
                        self.bad(&format!(
                            "{} is negative and '.uleb128' is unsigned",
                            residue.constant
                        ))
                    })?;
                    crate::unwind::uleb(&mut bytes, value);
                }
                self.put(&bytes)?;
                continue;
            }
            // A label further on is where the last pass put it, and the number is written in as
            // many bytes as that comes to, which the end of the file writes again once the places
            // stop moving. One that is no distance in a section at all gets the old four bytes and
            // is refused at the end.
            let width = match self.absolute(&sum) {
                Some(guess) => {
                    let mut bytes = Vec::new();
                    if signed {
                        crate::unwind::sleb(&mut bytes, guess);
                    } else {
                        crate::unwind::uleb(&mut bytes, u64::try_from(guess).unwrap_or(0));
                    }
                    u8::try_from(bytes.len()).expect("ten bytes at most")
                }
                None => LEB_ROOM,
            };
            let (part, at) = (self.here, self.at());
            self.put(&vec![0; usize::from(width)])?;
            self.fixups.push(Fixup {
                part,
                at,
                width,
                sum,
                reach: Reach::Near,
                slot: Reference::Got,
                branch: None,
                jump: false,
                field: None,
                leb: Some(signed),
                line: self.line,
            });
        }
        Ok(())
    }

    /// `.octa`, sixteen bytes of a number, which is how hand written vector code lays out a mask.
    ///
    /// Wider than any expression here, so a plain number is read at its own width and anything
    /// else is an expression that has to fit in sixty four bits and is sign extended.
    fn octa(&mut self, args: &[String]) -> Result<(), Trouble> {
        if args.is_empty() {
            return Err(self.bad("a data directive with nothing after it"));
        }
        for arg in args {
            let bytes = match wide(arg) {
                Some(value) => value.to_le_bytes(),
                None => i128::from(self.number(arg)?).to_le_bytes(),
            };
            self.put(&bytes)?;
        }
        Ok(())
    }

    /// `.ascii` and the two that add the terminator.
    fn text_bytes(&mut self, args: &[String], terminated: bool) -> Result<(), Trouble> {
        for arg in args {
            // Strings next to each other are one string, as gas reads them, so `"a" "b"` is `ab`
            // and `.asciz` ends it once. The kernel's `EXPORT_SYMBOL` writes `.ascii "" "\0"`.
            let mut bytes = Vec::new();
            for piece in adjacent(arg.trim()) {
                bytes.extend(self.string(piece)?);
            }
            if terminated {
                bytes.push(0);
            }
            self.put(&bytes)?;
        }
        Ok(())
    }

    /// `.incbin "file"`, with an optional number of bytes to skip and then to take.
    ///
    /// The name is read from where the assembler runs, which is what gas does first. The kernel
    /// uses it to put the real mode blob into `rmpiggy.S`, and kbuild runs every compile from the
    /// top of the build tree, where that name leads.
    fn incbin(&mut self, args: &[String]) -> Result<(), Trouble> {
        let Some(name) = args.first() else {
            return Err(self.bad(".incbin with no file named"));
        };
        if args.len() > 3 {
            return Err(self.bad(".incbin wants a file, and an optional skip and count"));
        }
        let name = String::from_utf8_lossy(&self.string(name.trim())?).into_owned();
        let bytes = std::fs::read(&name)
            .map_err(|e| self.bad(&format!(".incbin could not read {name}: {e}")))?;
        let skip = match args.get(1) {
            Some(arg) => self.size(arg)?,
            None => 0,
        };
        let skip = usize::try_from(skip).unwrap_or(usize::MAX);
        if skip > bytes.len() {
            let what =
                format!(".incbin skipping {skip} bytes of {name}, which has {}", bytes.len());
            return Err(self.bad(&what));
        }
        let left = bytes.len() - skip;
        let count = match args.get(2) {
            Some(arg) => usize::try_from(self.size(arg)?).unwrap_or(usize::MAX),
            None => left,
        };
        if count > left {
            let what =
                format!(".incbin of {count} bytes from {name}, which has {left} past the skip");
            return Err(self.bad(&what));
        }
        self.put(&bytes[skip..skip + count])
    }

    /// `.align`, `.balign` and `.p2align`, which differ only in what the first number means.
    ///
    /// On x86-64 ELF `.align` counts bytes, which is the trap: on AArch64, and on Apple's platforms
    /// whatever the machine, the same directive is a power of two the way `.p2align` is. gcc writes
    /// `.align 3` for eight bytes on AArch64, and read as bytes that is not an alignment at all.
    fn align(&mut self, word: &str, args: &[String]) -> Result<(), Trouble> {
        let Some(head) = args.first() else {
            return Err(self.bad(&format!(".{word} with nothing after it")));
        };
        let first = self.number(head)?;
        let first = self.count(first)?;
        let powers = word == "p2align" || (word == "align" && (self.aarch64 || self.macho));
        let boundary = if powers {
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
        // A fill of `0x90` in code is asking for no-ops, and gas takes it as asking for the same
        // long ones it writes when there is no fill at all. GMP aligns every loop this way.
        let fill = fill.filter(|&fill| !(exec && !self.aarch64 && fill == 0x90));
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
        if boundary > 1 {
            self.lines.variable(self.here, at, at + need);
        }
        if need == 0 && most.is_some_and(|most| padding(at, boundary, None) > most) {
            return Ok(());
        }
        let part = &mut self.parts[self.here];
        part.align = part.align.max(boundary);
        match fill {
            Some(fill) => self.pad(need, fill),
            // Not one byte at a time, which is what gas does as well: the padding in front of a
            // loop is fallen into, and a few long nops are fewer instructions than many short ones.
            // On AArch64 the no-op is a word, and padding that is not a whole number of words is
            // zeros up to the next one, which nothing can be walking through. An i386 object has
            // no-ops of its own, which `nops_i386` knows, and gas pads with those or with x86-64's
            // by the machine the object is for, whatever `.code32` or `.code64` said.
            None if exec && self.aarch64 => {
                let mut bytes = vec![0u8; (need % 4) as usize];
                for _ in 0..need / 4 {
                    bytes.extend_from_slice(&A64_NOP.to_le_bytes());
                }
                self.put(&bytes)
            }
            None if exec => {
                let mut bytes = Vec::new();
                let mut need = usize::try_from(need).unwrap_or(usize::MAX);
                // After data, gas starts with a `nop` of one byte, so that a byte of the data that
                // looks like a prefix runs into that and not into the long no-op after it. The
                // section remembers this across alignments and across switching away and back.
                // gas 2.40 does not, and pads after data as it does after code.
                if need > 0
                    && !self.before_2_42
                    && self.after_data.contains(&self.section_of(self.here))
                {
                    bytes.push(0x90);
                    need -= 1;
                }
                if self.before_2_42 {
                    // gas 2.40 picks its no-ops when the file is over. See `old_padding`.
                    self.old_padding.push((self.here, self.at(), need));
                    bytes.resize(need, 0x90);
                } else if self.sixteen.is_some() {
                    crate::sixteen::nops(need, &mut bytes);
                } else if self.i386 {
                    nops_i386(self.mode(), need, &mut bytes);
                } else {
                    nops(need, &mut bytes);
                }
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
    ///
    /// gas also takes the name and the type with only a space between them, `.type foo STT_FUNC`,
    /// which is how the kernel's crypto code copied from OpenSSL writes it.
    fn type_directive(&mut self, args: &[String]) -> Result<(), Trouble> {
        let spaced: Vec<String>;
        let args = match args {
            [one] if one.trim().contains(char::is_whitespace) => {
                spaced = one.split_whitespace().map(str::to_owned).collect();
                &spaced[..]
            }
            _ => args,
        };
        let [name, what] = self.two(args, ".type")?;
        let what = unquoted(what.trim().trim_start_matches(['@', '%']));
        let sort = match what.trim_start_matches("STT_").to_ascii_lowercase().as_str() {
            "func" | "function" => Sort::Func,
            "object" | "gnu_unique_object" => Sort::Object,
            "tls_object" | "tls" => Sort::Thread,
            "notype" | "" => Sort::Untyped,
            // An ifunc, which gcc writes for `ifunc` and `target_clones`. Only ELF has the type,
            // and the object writer refuses it on anything else.
            "gnu_indirect_function" => Sort::Ifunc,
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
            // Apple's assembler takes the alignment as a power of two, the way `.p2align` does.
            Some(arg) if self.macho => {
                let power = self.number(&arg.clone())?;
                let power = self.count(power)?;
                if power > 15 {
                    return Err(self.bad(&format!("an alignment of 2^{power} is too large")));
                }
                1 << power
            }
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
            let (was, before) = (self.here, self.before);
            if self.macho {
                self.apple_section("__DATA", "__bss", None, &[])?;
            } else {
                // gas's `bss_alloc` puts it in subsection one, so every local common comes after
                // whatever the file put in `.bss` itself, wherever in the file the line was.
                self.section(".bss", Shape::of(".bss"));
                self.subsection(1);
            }
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
            self.pooled.entry(at).or_default().push((sym, align));
            // Back where the file was, with `.previous` where it was too, since gas moves there
            // and back without a word to the section stack.
            self.here = was;
            self.before = before;
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
    ///
    /// Labels are allowed in it as long as they cancel, since a distance between two places in one
    /// section is a number. See [`Reader::guesses`] for one that is further down the file.
    fn number(&mut self, text: &str) -> Result<i64, Trouble> {
        let sum = self.expression(text)?;
        self.absolute(&sum).ok_or_else(|| self.not_number(text))
    }

    /// What is wrong with an expression that had to be a number and was not one.
    fn not_number(&self, text: &str) -> Trouble {
        self.bad(&format!("'{}' has to be a number here and it names something", text.trim()))
    }

    /// A count of bytes, which a file may work out from labels.
    ///
    /// A negative number the file wrote is refused. A negative distance between labels is nothing,
    /// which is what gas makes of it: the padding of an alternative whose replacement is shorter
    /// than the original is exactly that, and it comes out as no padding.
    fn size(&mut self, text: &str) -> Result<u64, Trouble> {
        self.sizing = true;
        let size = self.sized(text);
        self.sizing = false;
        size
    }

    /// [`Reader::size`], with [`Reader::sizing`] set.
    fn sized(&mut self, text: &str) -> Result<u64, Trouble> {
        let guessed = self.guessed.len();
        let sum = self.expression(text)?;
        // A sum that is flat may still have come from labels, when an operator in it had to settle
        // them to get a number, and a guess that is wrong can make it negative for a pass.
        let value = match sum.flat() {
            Some(value) if self.guessed.len() == guessed => self.count(value)?,
            Some(value) => u64::try_from(value).unwrap_or(0),
            None => {
                let value = self.absolute(&sum).ok_or_else(|| self.not_number(text))?;
                u64::try_from(value).unwrap_or(0)
            }
        };
        self.not_runaway(value, guessed)?;
        Ok(value)
    }

    /// Refuses a count past [`MOST_GUESSED`] that rests on a place guessed since `guessed`.
    fn not_runaway(&self, value: u64, guessed: usize) -> Result<(), Trouble> {
        if value > MOST_GUESSED && self.guessed.len() > guessed {
            let what = format!(
                "this comes to {value} bytes from where labels further down were guessed to be, \
                 and a count that grows like that is one that depends on itself"
            );
            return Err(self.bad(&what));
        }
        Ok(())
    }

    /// Where `.org` goes to, as an offset into the current section.
    ///
    /// A number is one already. So is a place in the current section, which is how a file says
    /// `.org . + (2f - 1f)` or `.org 0b + 16`, and it is as far into the section as it is.
    fn origin(&mut self, text: &str) -> Result<u64, Trouble> {
        let guessed = self.guessed.len();
        let sum = self.expression(text)?;
        let value = match sum.flat() {
            Some(value) => value,
            None => match self.placed(&sum) {
                Some((constant, net)) if net.is_empty() => constant,
                Some((constant, net))
                    if net.len() == 1 && net.get(&self.here).copied() == Some(1) =>
                {
                    constant
                }
                _ => {
                    let what = format!(
                        "'{}' is not a place in this section, so there is nowhere for .org to go",
                        text.trim()
                    );
                    return Err(self.bad(&what));
                }
            },
        };
        let value = self.count(value)?;
        self.not_runaway(value, guessed)?;
        Ok(value)
    }

    /// The number a sum comes to, if the labels in it cancel.
    fn absolute(&mut self, sum: &Sum) -> Option<i64> {
        if let Some(value) = sum.flat() {
            return Some(value);
        }
        let (constant, net) = self.placed(sum)?;
        net.is_empty().then_some(constant)
    }

    /// What a sum comes to with every name in it given a place, noting down the places that were
    /// guessed.
    fn placed(&mut self, sum: &Sum) -> Option<(i64, BTreeMap<usize, i64>)> {
        let mut guessed = Vec::new();
        let found = self.evaluate(sum, false, &mut guessed, 0);
        let line = self.line;
        self.guessed.extend(guessed.into_iter().map(|(name, held)| (name, held, line)));
        found
    }

    /// What a sum comes to with every name in it given a place: a number, and how many times the
    /// start of each section is still counted in it, with the sections it cancels out of left out.
    ///
    /// A name is where this pass put it if it has got there, and otherwise where the last pass
    /// put it, which goes into `guessed`. `raw` says the names are as the file wrote them rather
    /// than the entries they mean, which is so for an expression still being parsed. Nothing for
    /// a name that is not a place in this file at all.
    fn evaluate(
        &self,
        sum: &Sum,
        raw: bool,
        guessed: &mut Vec<(String, Held)>,
        depth: usize,
    ) -> Option<(i64, BTreeMap<usize, i64>)> {
        let mut constant = sum.constant;
        let mut net: BTreeMap<usize, i64> = BTreeMap::new();
        for term in &sum.terms {
            let (part, offset) = match &term.what {
                What::Here { part, at } => (*part, *at),
                What::Symbol(name) => match self.place(name, raw, guessed, depth)? {
                    Held::Absolute(value) => {
                        constant = constant.wrapping_add(term.coeff.wrapping_mul(value as i64));
                        continue;
                    }
                    Held::In { part, offset } => (part, offset as i64),
                    Held::Common { .. } | Held::Undefined => return None,
                },
            };
            constant = constant.wrapping_add(term.coeff.wrapping_mul(offset));
            *net.entry(part).or_insert(0) += term.coeff;
        }
        net.retain(|_, coeff| *coeff != 0);
        Some((constant, net))
    }

    /// Where one name is on this pass, or where it was on the last one when this pass has not got
    /// to it yet. See [`Reader::evaluate`].
    ///
    /// A name set to an expression is worked out from that expression, since a set is only given
    /// its value at the end of the file and a count that names one is wanted now. A label with no
    /// place on the last pass either, which is every one further down on the first pass, is taken
    /// to be here, and the next pass puts it right.
    fn place(
        &self,
        name: &str,
        raw: bool,
        guessed: &mut Vec<(String, Held)>,
        depth: usize,
    ) -> Option<Held> {
        let held = if raw { self.named(name).ok()? } else { name.to_owned() };
        if let Some(&sym) = self.known.get(&held) {
            match self.syms[sym].at {
                at @ (Held::In { .. } | Held::Absolute(_)) => {
                    return Some(self.unrelaxed(&held, at));
                }
                Held::Common { .. } => return None,
                Held::Undefined => {
                    if let Some((_, sum, _)) = self.sets.iter().find(|(set, ..)| *set == sym) {
                        // Deep enough for any chain of sets a file writes, and short of looping
                        // for ever on two that name each other, which the end of the file refuses.
                        if depth > 64 {
                            return None;
                        }
                        let (constant, net) = self.evaluate(sum, false, guessed, depth + 1)?;
                        let net: Vec<(usize, i64)> = net.into_iter().collect();
                        return match net.as_slice() {
                            [] => Some(Held::Absolute(constant as u64)),
                            [(part, 1)] => Some(Held::In { part: *part, offset: constant as u64 }),
                            _ => None,
                        };
                    }
                }
            }
        }
        // The name of a section is where it starts, which the end of the last pass recorded under
        // the name it gives the section's start, and which is known once the section is open.
        // Either is still a guess, which the end of the pass checks, in case a label of that
        // name turns up further down.
        let section = || {
            let start = self.guesses.get(&format!("{held}\u{1}start")).copied();
            let open = self.named.get(&held).map(|&part| Held::In { part, offset: 0 });
            start.or(open).filter(|_| self.elf())
        };
        let guess = self
            .guesses
            .get(&held)
            .copied()
            .or_else(section)
            .unwrap_or(Held::In { part: self.here, offset: self.at() });
        let seen = self.unrelaxed(&held, guess);
        guessed.push((held, guess));
        Some(seen)
    }

    /// A place as a count on [`Reader::first_round`] sees it, which for one in a section further
    /// down the list is how far it is into its piece.
    fn unrelaxed(&self, name: &str, at: Held) -> Held {
        let Held::In { part, offset } = at else { return at };
        if !self.first_round || !self.sizing || part <= self.here {
            return at;
        }
        let Some(&within) = self.within.get(name) else { return at };
        if within != offset {
            self.first_round_counted.set(true);
        }
        Held::In { part, offset: within }
    }

    /// Whether every place this pass guessed is where the pass put it, which is what makes the pass
    /// the answer.
    ///
    /// A name that turned out not to be a place in this file at all is refused on the line that
    /// wanted a number out of it, since no number of passes will give it one.
    fn guesses_held(&self) -> Result<bool, Trouble> {
        let now =
            |name: &String| self.known.get(name).map_or(Held::Undefined, |&sym| self.syms[sym].at);
        for (name, _, line) in &self.guessed {
            if !matches!(now(name), Held::In { .. } | Held::Absolute(_)) {
                let shown = name.split('\u{1}').next().unwrap_or(name);
                let why = format!(
                    "'{shown}' is not a place in this file, and it is in something that has to be \
                     a number"
                );
                return Err(Trouble { line: *line, why });
            }
        }
        Ok(self.guessed.iter().all(|(name, guess, _)| now(name) == *guess))
    }

    /// Where every name is at the end of this pass, for the next one to guess from.
    /// How far into its piece every label is. See [`Reader::first_round`].
    fn within_pieces(&self) -> Map<String, u64> {
        self.pieces
            .iter()
            .filter_map(|(&sym, &piece)| match self.syms[sym].at {
                Held::In { part, offset } => Some((
                    self.syms[sym].name.clone(),
                    offset.saturating_sub(self.lines.start(part, piece)),
                )),
                _ => None,
            })
            .collect()
    }

    fn places(&self) -> Map<String, Held> {
        self.syms
            .iter()
            .filter(|sym| matches!(sym.at, Held::In { .. } | Held::Absolute(_)))
            .map(|sym| (sym.name.clone(), sym.at))
            .collect()
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
        let mut guessed = Vec::new();
        let parsed = Parser {
            text: text.trim(),
            at: 0,
            here,
            values: Some(&self.values),
            reader: Some(&*self),
            guessed: Some(&mut guessed),
        }
        .whole();
        let line = self.line;
        self.guessed.extend(guessed.into_iter().map(|(name, held)| (name, held, line)));
        let mut sum = parsed.map_err(|why| Trouble { line, why })?;
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

    /// The padding gas 2.40 writes in code, which it picks by the mode the file ends in rather than
    /// the one each run is in, and by the machine the object is for. See [`nops_before_2_42`].
    fn old_padding(&mut self) {
        let table = if self.sixteen.is_some() {
            OldNops::Sixteen
        } else if self.i386 {
            OldNops::I386
        } else {
            OldNops::X86_64
        };
        for &(part, at, need) in &self.old_padding {
            let mut bytes = Vec::with_capacity(need);
            nops_before_2_42(table, need, &mut bytes);
            let at = usize::try_from(at).expect("a place in memory");
            self.parts[part].bytes[at..at + need].copy_from_slice(&bytes);
        }
    }

    /// Work out everything that was waiting for the end of the file.
    ///
    /// Or what the next pass has to do differently, when this one is not the answer: the branches
    /// written short that do not reach, for the file to be read again with those long, and where
    /// everything ended up, for a count that guessed at a place further down to be worked out
    /// again from where it really is.
    fn finish(mut self) -> Result<Result<Assembled, Again>, Trouble> {
        if self.frame.is_some() {
            return Err(self.bad("a '.cfi_startproc' that is never ended"));
        }
        if self.described.is_some() {
            return Err(self.bad("a '.seh_proc' that is never ended"));
        }
        let within = self.within_pieces();
        self.old_padding();
        self.join_subsections();
        self.sections_by_name();
        self.line_table()?;
        self.unwind_table();
        self.seh_table()?;
        self.resolve_sets()?;
        let held = self.guesses_held()?;
        if held {
            if let Some(doubt) = self.doubts.first() {
                return Err(doubt.clone());
            }
        }
        self.resolve_sizes()?;
        self.copy_attributes();
        self.versions()?;
        let grow = self.too_far()?;
        let first_round = self.first_round_counted.get();
        if !grow.is_empty() || !held || first_round {
            let line = self.guessed.first().map_or(self.line, |(_, _, line)| *line);
            return Ok(Err(Again { grow, places: self.places(), line, within, first_round }));
        }
        self.resolve_fixups()?;
        for reloc in self.parts.iter_mut().flat_map(|part| part.relocs.iter_mut()) {
            if let Some(renamed) = self.renamed.get(&reloc.symbol) {
                reloc.symbol.clone_from(renamed);
            }
        }
        self.last_settings();
        self.leaders()?;
        let marker = ".note.GNU-stack";
        if self.noexecstack && self.elf() && !self.parts.iter().any(|part| part.name == marker) {
            self.section(marker, Shape { bits: true, ..Shape::default() });
            self.declared.insert(self.here);
        }
        // A section the file only ever mentioned by its short name is dropped. One an ELF
        // `.section` named is kept however empty, as gas keeps it, since a linker script may keep
        // it and take its address, and so are the three gas makes before it reads anything.
        let keep: Vec<bool> = self
            .parts
            .iter()
            .enumerate()
            .map(|(at, part)| {
                !self.subs.contains_key(&at)
                    && (part.size > 0
                        || (self.elf() && at < 3)
                        || !part.relocs.is_empty()
                        || self.labelled.contains(&at)
                        || self.declared.contains(&at))
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
            // gas makes anything in a thread-local section thread-local, whatever `.type` said,
            // and gcc's i386 listing says `@object` for a `__thread` variable. A linker refuses a
            // thread-local reference to a name another object defines as anything else.
            let sort = match at {
                Held::In { part, .. }
                    if self.i386
                        && parts[part].shape.thread
                        && matches!(sym.sort, Sort::Untyped | Sort::Object) =>
                {
                    Sort::Thread
                }
                _ => sym.sort,
            };
            names.push(Name {
                name: sym.name,
                at,
                size: sym.size,
                sort,
                binding,
                visibility: sym.visibility,
            });
        }
        Ok(Ok(Assembled { parts, names, subsections: self.subsections }))
    }

    /// `.file 1 "dir" "name" md5 0x...`, which puts a file in the line table. The directory and the
    /// sum may each be left out.
    fn numbered_file(&mut self, rest: &str) -> Result<(), Trouble> {
        let words = words(rest).map_err(|why| self.bad(&why))?;
        let Some((first, words)) = words.split_first() else {
            return Err(self.bad(".file with nothing after it"));
        };
        let number = self.number(first)?;
        let number = self.count(number)?;
        let mut quoted = Vec::new();
        let mut words = words.iter();
        let mut md5 = None;
        while let Some(word) = words.next() {
            if word.starts_with('"') && quoted.len() < 2 {
                quoted.push(self.string(word)?);
            } else if word == "md5" {
                let sum = words.next().and_then(|sum| wide(sum));
                let Some(sum) = sum else {
                    return Err(self.bad("md5 wants a 128 bit number after it"));
                };
                md5 = Some(sum.to_be_bytes());
            } else {
                return Err(self.bad(&format!("'{word}' after .file, which is not a name or md5")));
            }
        }
        let (dir, name) = match quoted.len() {
            1 => (None, quoted.remove(0)),
            2 => {
                let name = quoted.remove(1);
                (Some(quoted.remove(0)), name)
            }
            _ => return Err(self.bad(".file with a number and no name")),
        };
        self.lines.file(number, dir, name, md5).map_err(|why| self.bad(&why))
    }

    /// `.loc file line [column] [what else]`, which says the instructions from here on came from
    /// that line, until the next `.loc`.
    fn loc(&mut self, rest: &str) -> Result<(), Trouble> {
        // A `.loc` that no instruction came after is a row where the next one starts.
        self.row()?;
        let words = words(rest).map_err(|why| self.bad(&why))?;
        let [file, line, words @ ..] = words.as_slice() else {
            return Err(self.bad(".loc wants a file and a line"));
        };
        let file = self.number(file)?;
        let file = self.count(file)?;
        let line = self.number(line)?;
        let line = self.count(line)?;
        let mut words = words.iter().peekable();
        let mut loc = std::mem::take(&mut self.lines.current);
        loc.file = file;
        loc.line = line;
        loc.discriminator = 0;
        if let Some(column) = words.next_if(|word| word.starts_with(|c: char| c.is_ascii_digit())) {
            let column = self.number(column)?;
            loc.column = self.count(column)?;
        }
        while let Some(word) = words.next() {
            let mut value = |what: &str| match words.next() {
                Some(value) => Ok(value.clone()),
                None => Err(format!("{what} wants a value after it")),
            };
            match word.as_str() {
                "basic_block" => loc.basic_block = true,
                "prologue_end" => loc.prologue_end = true,
                "epilogue_begin" => loc.epilogue_begin = true,
                "is_stmt" | "isa" | "discriminator" => {
                    let text = value(word).map_err(|why| self.bad(&why))?;
                    let number = self.number(&text)?;
                    let number = self.count(number)?;
                    match word.as_str() {
                        "is_stmt" if number > 1 => {
                            return Err(self.bad("is_stmt value not 0 or 1"));
                        }
                        "is_stmt" => loc.stmt = number == 1,
                        "isa" => loc.isa = number,
                        _ => loc.discriminator = number,
                    }
                }
                "view" => {
                    let text = value(word).map_err(|why| self.bad(&why))?;
                    // A number only says the view there is zero, and `-0` makes it so.
                    let reset = text == "-0";
                    if !reset && text.starts_with(|c: char| c.is_ascii_digit()) {
                        if self.number(&text)? != 0 {
                            return Err(self.bad("numeric view can only be asserted to zero"));
                        }
                        loc.view = Some(String::new());
                    } else if reset {
                        loc.view = Some(String::new());
                        loc.reset = true;
                    } else {
                        loc.view = Some(text);
                    }
                }
                _ => {
                    let what = format!("unknown .loc sub-directive `{word}'");
                    return Err(self.bad(&what));
                }
            }
        }
        let immediate = loc.view.is_some();
        self.lines.current = loc;
        self.lines.waiting = true;
        self.lines.seen = true;
        if immediate {
            self.row()?;
        }
        Ok(())
    }

    /// The row the last `.loc` is waiting to make, here, if there is one, with the name of its view
    /// set to the view's number.
    fn row(&mut self) -> Result<(), Trouble> {
        let shape = self.parts[self.here].shape;
        let code = shape.exec && shape.alloc && (shape.bits || !self.elf());
        let at = self.at();
        if let Some((name, view)) = self.lines.row(self.here, at, code) {
            if !name.is_empty() {
                let sym = self.sym(&name);
                self.syms[sym].at = Held::Absolute(view);
            }
        }
        Ok(())
    }

    /// The line table, into `.debug_line`, with the names of the files and directories it uses on
    /// the end of `.debug_line_str`.
    ///
    /// gas writes it when the file has a `.debug_info` with something in it, which is gcc's under
    /// `-g`, and leaves `.debug_line` alone when the file wrote one itself and said no `.loc`. Both
    /// at once is a mistake gas refuses. Only for x86 ELF, which is all gcc's output this reads is.
    fn line_table(&mut self) -> Result<(), Trouble> {
        if !self.elf() || self.aarch64 {
            return Ok(());
        }
        let size = |name: &str| self.named.get(name).map_or(0, |&at| self.parts[at].size);
        if size(".debug_info") == 0 {
            return Ok(());
        }
        if size(".debug_line") != 0 {
            if self.lines.any_rows() && self.lines.seen {
                return Err(self.bad("duplicate .debug_line sections"));
            }
            return Ok(());
        }
        let address = if self.i386 { 4 } else { 8 };
        let parts = &self.parts;
        let table =
            self.lines.table(address, |part| parts[part].size).map_err(|why| self.bad(&why))?;
        let here = self.here;
        let mut strings = 0;
        if !table.strings.is_empty() {
            let shape = Shape { bits: true, merge: 1, strings: true, ..Shape::default() };
            self.section(".debug_line_str", shape);
            strings = self.at();
            self.put(&table.strings)?;
        }
        self.section(".debug_line", Shape { bits: true, ..Shape::default() });
        let (part, start) = (self.here, self.at());
        self.put(&table.bytes)?;
        let text = self.named.get(".debug_line_str").copied();
        for (n, hole) in table.holes.iter().enumerate() {
            let (to, offset) = match hole.to {
                lines::To::Code { part, at } => (part, at),
                lines::To::Text(at) => (text.unwrap_or(part), strings + at as u64),
            };
            let name = format!("\u{1}line{n}");
            let sym = self.sym(&name);
            self.syms[sym].at = Held::In { part: to, offset };
            self.fixups.push(Fixup {
                part,
                at: start + hole.at as u64,
                width: hole.width,
                sum: Sum { constant: 0, terms: vec![Term { coeff: 1, what: What::Symbol(name) }] },
                reach: Reach::Near,
                slot: Reference::Got,
                branch: None,
                jump: false,
                field: None,
                leb: None,
                line: self.line,
            });
        }
        self.go(here);
        Ok(())
    }

    /// The unwind table the frame rules describe, as a section of its own.
    ///
    /// Written only when a file said some rules, which is every function the compiler emits and
    /// every function gcc does. A file of assembly written by hand with none gets no table, the same
    /// as it does from gas.
    fn unwind_table(&mut self) {
        if self.frames.is_empty() {
            return;
        }
        if !self.no_unwind {
            self.eh_table();
        }
        // After the unwind table, which is the order gas writes the two in.
        if self.debugger && !self.macho {
            self.debugger_table();
        }
    }

    /// The unwind table, in `.eh_frame` or the Mac's `__eh_frame`.
    fn eh_table(&mut self) {
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
                hooked: 0,
                landings: Vec::new(),
            })
            .collect();
        let rows: Vec<_> = self.frames.iter().map(|frame| frame.rows.clone()).collect();
        let named: Vec<Option<Named>> = self
            .frames
            .iter()
            .map(|frame| {
                let personality = frame.personality.clone()?;
                Some(Named { personality, lsda: frame.lsda.clone() })
            })
            .collect();
        let conv = self.conv();
        let format = if self.macho { ObjectFormat::MachO } else { ObjectFormat::Elf };
        let begins: Vec<_> = self.frames.iter().map(|frame| frame.begins).collect();
        let Ok(table) = crate::unwind::table(&funcs, &rows, conv, format, &named, &begins) else {
            return;
        };
        for frame in &self.frames {
            self.relocated.insert(frame.sym);
        }
        // Where clang puts it on a Mac, with the flags it gives it: records the linker may merge
        // with another file's, and a section it keeps whenever it keeps the code the records
        // point at, rather than one it drops because nothing names it.
        let (name, shape) = if self.macho {
            let attributes = ["no_toc", "strip_static_syms", "live_support"];
            let Ok(shape) = Shape::mach("__TEXT", "__eh_frame", Some("coalesced"), &attributes)
            else {
                return;
            };
            ("__TEXT,__eh_frame", shape)
        } else {
            (".eh_frame", Shape { alloc: true, bits: true, ..Shape::default() })
        };
        let size = table.bytes.len() as u64;
        self.parts.push(Part {
            name: name.to_owned(),
            bytes: table.bytes,
            size,
            // A pointer, which is what every record is padded to and what gas aligns it to.
            align: u64::from(conv.word),
            shape,
            relocs: table.relocs,
            group: None,
            link: None,
        });
    }

    /// The same rules as `.debug_frame`, for a file whose `.cfi_sections` asked for them there.
    ///
    /// Written by the same code that writes the copy for a `-g` build with no unwind table, which
    /// is the shape gas gives it: a header of version one with no augmentation, and records that
    /// name their header by its offset in the section and their function by its address, both
    /// left to the linker. Not loaded, so not allocated, and aligned to a pointer like gas does.
    fn debugger_table(&mut self) {
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
                hooked: 0,
                landings: Vec::new(),
            })
            .collect();
        let rows: Vec<_> = self.frames.iter().map(|frame| frame.rows.clone()).collect();
        let conv = self.conv();
        let begins: Vec<_> = self.frames.iter().map(|frame| frame.begins).collect();
        let Some(mut table) =
            crate::unwind::debug_frame(&funcs, &rows, conv, ObjectFormat::Elf, &begins)
        else {
            return;
        };
        for frame in &self.frames {
            self.relocated.insert(frame.sym);
        }
        // A record names its header by the section's own name, which the object writer only knows
        // as a name something defines. So the front of the section gets a name of its own, one
        // nothing else can spell, and the writer turns it back into the section as gas has it.
        let front = self.sym("\u{1}debug_frame");
        self.syms[front].at = Held::In { part: self.parts.len(), offset: 0 };
        self.relocated.insert(front);
        for reloc in &mut table.relocs {
            if reloc.symbol == table.name {
                reloc.symbol.clone_from(&self.syms[front].name);
            }
        }
        let size = table.bytes.len() as u64;
        self.parts.push(Part {
            name: table.name,
            bytes: table.bytes,
            size,
            align: u64::from(conv.word),
            shape: Shape { bits: true, ..Shape::default() },
            relocs: table.relocs,
            group: None,
            link: None,
        });
    }

    /// The table Windows reads, as `.seh_` directives described it: the descriptions in `.xdata`
    /// and a row for each function in `.pdata`, which is where the object writer puts the same two.
    ///
    /// Written by the same code the object writer uses, from the same codes, so the only thing
    /// that differs is how the rows name their functions: by the entry made at `.seh_proc` rather
    /// than by the function's own name, which the linker resolves to the same address.
    fn seh_table(&mut self) -> Result<(), Trouble> {
        if self.prologues.is_empty() {
            return Ok(());
        }
        let funcs: Vec<Extent> = self
            .prologues
            .iter()
            .map(|described| Extent {
                name: self.seh_name(described),
                start: described.start as usize,
                len: described.len as usize,
                align: 1,
                binding: Binding::Local,
                visibility: Visibility::Default,
                patch: None,
                hooked: 0,
                landings: Vec::new(),
            })
            .collect();
        let table = if self.aarch64 {
            let procs: Vec<crate::xdata::Proc> =
                self.prologues.iter().map(|described| described.arm.clone()).collect();
            crate::xdata::table(&funcs, &procs)
        } else {
            let prologues: Vec<Prologue> =
                self.prologues.iter().map(|described| described.prologue.clone()).collect();
            crate::unwind::seh(&funcs, &prologues)
        };
        let table = table.map_err(|error| {
            let error = match error {
                crate::Error::Frame { func, why } => {
                    let named = self.prologues.iter().find(|d| self.seh_name(d) == func);
                    crate::Error::Frame { func: named.map_or(func, |d| d.name.clone()), why }
                }
                other => other,
            };
            self.bad(&error.to_string())
        })?;
        for at in 0..self.prologues.len() {
            if self.seh_name(&self.prologues[at]) != self.prologues[at].name {
                self.relocated.insert(self.prologues[at].sym);
            }
        }
        let shape = Shape { alloc: true, bits: true, ..Shape::default() };
        let codes = self.parts.len();
        let size = table.info.len() as u64;
        self.parts.push(Part {
            name: ".xdata".to_owned(),
            bytes: table.info,
            size,
            align: 4,
            shape,
            relocs: Vec::new(),
            group: None,
            link: None,
        });
        // Each description's name, which a row reaches it through, and a local one, as it is in
        // an object the compiler writes.
        for label in table.labels {
            let sym = self.sym(&label.name);
            self.syms[sym].at = Held::In { part: codes, offset: label.at as u64 };
            self.relocated.insert(sym);
        }
        let size = table.bytes.len() as u64;
        self.parts.push(Part {
            name: ".pdata".to_owned(),
            bytes: table.bytes,
            size,
            align: 4,
            shape,
            relocs: table.relocs,
            group: None,
            link: None,
        });
        Ok(())
    }

    /// What a row names its function by: the name `.seh_proc` gave when that name is where the
    /// directive was, which is what the object writer names it by too, and otherwise the entry
    /// made at the directive.
    fn seh_name(&self, described: &Described) -> String {
        let here = Held::In { part: described.part, offset: described.start };
        match self.known.get(&described.name) {
            Some(&sym) if self.syms[sym].at == here => described.name.clone(),
            _ => self.syms[described.sym].name.clone(),
        }
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

    /// The type and size a name set to another name takes from it, where it was not given its own.
    fn copy_attributes(&mut self) {
        for (sym, target) in std::mem::take(&mut self.copies) {
            let Some(&target) = self.known.get(&target) else { continue };
            let (sort, size) = (self.syms[target].sort, self.syms[target].size);
            let alias = &mut self.syms[sym];
            if alias.sort == Sort::Untyped {
                alias.sort = sort;
            }
            if alias.size == 0 {
                alias.size = size;
            }
        }
    }

    /// The value a name set more than once is written to the table with, which is the last one.
    ///
    /// A setting after the first is its own entry, so that a line between the two reads the value
    /// the name had then (see [`Reader::assign`]), and that left the first value under the name.
    /// gas copies the symbol when it is set again and the copy is what keeps the old value, so the
    /// name in its table has the last one. The kernel's crypto code counts with `.set i, i+1` round
    /// a `.rept`, and every one of those units had `i` at 0 where gas has the count. A name a
    /// relocation points at is left as it is, since the relocation was written against the value
    /// it had then.
    fn last_settings(&mut self) {
        let relocated: Set<&str> = self
            .parts
            .iter()
            .flat_map(|part| part.relocs.iter().map(|reloc| reloc.symbol.as_str()))
            .collect();
        for (name, last) in &self.current {
            if name == last || relocated.contains(name.as_str()) {
                continue;
            }
            let (Some(&first), Some(&last)) = (self.known.get(name), self.known.get(last)) else {
                continue;
            };
            let placed = |at: Held| matches!(at, Held::In { .. } | Held::Absolute(_));
            if placed(self.syms[first].at) && placed(self.syms[last].at) {
                self.syms[first].at = self.syms[last].at;
            }
        }
    }

    /// The versioned names `.symver` asked for, measured against gas 2.42.
    ///
    /// `name2@VERS` and `name2@@VERS` are a second symbol at the place the first one is, with its
    /// type, size, binding and visibility, and the first one stays. `name2@@@VERS` renames the
    /// first one to `name2@@VERS`, so nothing is left under the old name. A name this file only
    /// refers to is renamed whichever spelling was used, since what is asked for then is a
    /// reference to that version and there is no place to put a copy. A name that turns up
    /// nowhere at all, which is what is left of a `static` function the compiler threw away, gets
    /// nothing, as in gas.
    ///
    /// The linker reads the `@` itself. LTP's sctp library versions `sctp_connectx` this way, and
    /// a static link takes the `@@` one as the plain name.
    fn versions(&mut self) -> Result<(), Trouble> {
        for (name, versioned, line) in std::mem::take(&mut self.symvers) {
            let Some(&first) = self.known.get(&name) else { continue };
            let renamed = versioned.replacen("@@@", "@@", 1);
            if self.known.contains_key(&renamed) {
                let why = format!("'{renamed}' is defined twice");
                return Err(Trouble { line, why });
            }
            if versioned.contains("@@@") || self.syms[first].at == Held::Undefined {
                // The old name stays in the map, since what was written against it is resolved
                // after this and has to find the entry that now carries the new name.
                self.known.insert(renamed.clone(), first);
                self.renamed.insert(name, renamed.clone());
                self.syms[first].name = renamed;
                continue;
            }
            let copy = Sym { name: renamed.clone(), numbered: false, ..self.syms[first].clone() };
            self.known.insert(renamed, self.syms.len());
            self.syms.push(copy);
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
                .reduce_kept(&fixup.sum, fixup.jump)
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
            if let Some(field) = fixup.field {
                self.field(&fixup, field)?;
                continue;
            }
            if let Some(signed) = fixup.leb {
                self.padded_leb(&fixup, signed)?;
                continue;
            }
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
            // A number in an instruction that is still a name is the address of the name, which
            // the linker has to write and which only fits in four bytes or more. A name less a
            // label that is somewhere in this file is the other thing the linker can write, as the
            // distance to the name from these bytes, and whether the label is in the right section
            // is for the arm below that writes it to say. Anything else left over, two names this
            // file does not define say, is not a number the linker writes into an instruction.
            let value = matches!(
                fixup.reach,
                Reach::Value
                    | Reach::Extended
                    | Reach::Offset
                    | Reach::Slot
                    | Reach::Tls(_)
                    | Reach::Section
            );
            let named =
                matches!(residue.left.as_slice(), [Left { coeff: 1, what: What::Symbol(_), .. }]);
            let distance = matches!(fixup.reach, Reach::Value | Reach::Extended)
                && matches!(
                    residue.left.as_slice(),
                    [
                        Left { coeff: 1, what: What::Symbol(_), .. },
                        Left { coeff: -1, at: Some(_), .. },
                    ] | [
                        Left { coeff: -1, at: Some(_), .. },
                        Left { coeff: 1, what: What::Symbol(_), .. },
                    ]
                );
            if value && !residue.left.is_empty() && !named && !distance {
                return Err(bad(
                    "a number in an instruction that is not a name and a constant once the names \
                     in this section are counted, which no relocation writes"
                        .to_owned(),
                ));
            }
            let (symbol, kind, addend, after) = match residue.left.as_slice() {
                // A distance from the table or a slot of it is about a name, and a number on its
                // own has neither.
                [] if matches!(fixup.reach, Reach::Offset | Reach::Slot) => {
                    return Err(bad(
                        "'@GOTOFF' or '@GOT' of something that comes out as a number, which has \
                         no place in the global offset table"
                            .to_owned(),
                    ));
                }
                [] if matches!(fixup.reach, Reach::Tls(_) | Reach::Section) => {
                    return Err(bad(
                        "a thread-local suffix on something that comes out as a number, which is \
                         not a variable any thread has"
                            .to_owned(),
                    ));
                }
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
                    let high = if matches!(fixup.reach, Reach::Branch | Reach::Plain) {
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
                // An instruction on sixty four bits sign extends four bytes of it, which the linker
                // is told so that it checks the address fits that way. See [`Reach::Extended`].
                //
                // On i386 the name may be how far something is from the global offset table, or a
                // slot of it, or the table itself, which gas writes as the distance to it from these
                // bytes whether or not the file took `.` away.
                [Left { coeff: 1, what: What::Symbol(name), .. }] => {
                    let kind = match fixup.reach {
                        Reach::Offset => Reference::GotOffset,
                        Reach::Slot => fixup.slot,
                        Reach::Tls(tls) => {
                            // gas marks a name a thread-local suffix reaches as thread-local,
                            // defined here or not, and a linker checks the two agree.
                            if let Some(&sym) = self.known.get(name) {
                                if matches!(self.syms[sym].sort, Sort::Untyped | Sort::Object) {
                                    self.syms[sym].sort = Sort::Thread;
                                }
                            }
                            Reference::Tls(tls)
                        }
                        Reach::Section => Reference::Section,
                        _ if self.i386 && name == TABLE => self.front(fixup.width, line)?,
                        Reach::Extended if fixup.width == 4 => Reference::Signed,
                        _ => Reference::Address { bytes: fixup.width },
                    };
                    (name.clone(), kind, residue.constant, 0)
                }
                // The distance from these bytes to something, which is what a position independent
                // table of offsets holds and what `.long foo - .` is asking for. The subtracted
                // side has to be these bytes or somewhere else in the same section, because a
                // distance to another section is not a number until the linker has laid both out.
                //
                // An instruction gets here too, with a number or a displacement that is a name
                // less a label of its own section. The kernel's decompressor writes
                // `leal ((gdt) - startup_32)(%ebp), %eax` to reach `gdt` from wherever it was
                // loaded, and gas writes that as the distance from these bytes to `gdt` with the
                // distance from `startup_32` to these bytes added on, which is this same addend.
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
                    // Four bytes or eight, and eight only from a directive, since no instruction
                    // counts eight bytes of distance. `.quad key - .` is how the kernel's jump
                    // label table says where each key is. Two is a jump or a call in sixteen bit
                    // code, or a `.word`, and one is a `.byte`, which the boot header has in front
                    // of everything else.
                    let wide = fixup.width == 8 && fixup.reach == Reach::Near;
                    let short = fixup.width == 2;
                    let tiny = fixup.width == 1;
                    if fixup.width != 4 && !wide && !short && !tiny {
                        return Err(bad(format!(
                            "a distance written into {} bytes, and one, two, four and eight are \
                             the only widths a relocation says one at",
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
                    let table = self.i386 && name == TABLE;
                    if table && fixup.reach != Reach::Near {
                        return Err(bad(format!(
                            "a branch to '{TABLE}', which is a table and not somewhere to go"
                        )));
                    }
                    let kind = if table {
                        self.front(fixup.width, line)?
                    } else if wide {
                        Reference::AwayWide
                    } else if short {
                        Reference::Short
                    } else if tiny {
                        Reference::Tiny
                    } else if fixup.reach == Reach::Branch && !near {
                        Reference::Call
                    } else {
                        Reference::Data
                    };
                    // The same distance said the other way, for the format that wants it apart
                    // from the addend rather than folded into it. See `rucc_object::Reloc`.
                    let after = (offset - fixup.at as i64 - i64::from(fixup.width)).max(0);
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
            // x86-64 has a relocation for an address in one byte and in two as well, which is
            // what `.byte sym` and `movw $sym, %ax` ask for and gas writes. The linker checks that
            // the address fits.
            let narrow = !self.aarch64 && !self.coff && !self.macho;
            if matches!(kind, Reference::Address { bytes }
                if bytes != 4 && bytes != 8 && !(narrow && bytes < 4))
            {
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

    /// The relocation an i386 file gets for the global offset table's own name, which is only ever
    /// written in four bytes.
    fn front(&self, width: u8, line: usize) -> Result<Reference, Trouble> {
        if width != 4 {
            let why = format!("'{TABLE}' written into {width} bytes, and it is only ever four");
            return Err(Trouble { line, why });
        }
        Ok(Reference::GotFront)
    }

    /// A LEB128 number that was waiting for a label further on, written into the bytes the pass
    /// gave it. See [`Reader::leb`].
    ///
    /// It has to come out as a number, since there is no relocation that writes one of these, and
    /// that is so for the distance between two labels in the same section, which is what one is.
    fn padded_leb(&mut self, fixup: &Fixup, signed: bool) -> Result<(), Trouble> {
        let line = fixup.line;
        let bad = |why: String| Trouble { line, why };
        let residue = self.reduce(&fixup.sum).map_err(bad)?;
        if !residue.left.is_empty() {
            return Err(bad(
                "a LEB128 number that is not a distance inside one section, which only the linker \
                 could work out and no relocation writes"
                    .to_owned(),
            ));
        }
        let value = residue.constant;
        if !signed && value < 0 {
            return Err(bad(format!("{value} is negative and '.uleb128' is unsigned")));
        }
        let room = 7 * u32::from(fixup.width);
        let fits = if room >= 63 {
            true
        } else if signed {
            (-(1i64 << (room - 1))..(1i64 << (room - 1))).contains(&value)
        } else {
            (0..(1i64 << room)).contains(&value)
        };
        if !fits {
            return Err(bad(format!("{value} does not fit in {} bytes of LEB128", fixup.width)));
        }
        let at = fixup.at as usize;
        let bytes = &mut self.parts[fixup.part].bytes[at..at + usize::from(fixup.width)];
        for (index, byte) in bytes.iter_mut().enumerate() {
            let seven = u8::try_from((value >> (7 * index)) & 0x7f).expect("seven bits");
            let more = if index + 1 < usize::from(fixup.width) { 0x80 } else { 0 };
            *byte = seven | more;
        }
        Ok(())
    }

    /// A field of an AArch64 instruction, filled in here when it is a distance within the section
    /// and left to the linker otherwise.
    ///
    /// Only a distance is filled in. The page `adrp` names and the low bits an `add` or a load
    /// carries are parts of an address, and nothing has an address until the linker has placed
    /// it, so those are a relocation even against a label in the same section, which is what gas
    /// writes for them too. The addend is the constant the instruction was written with, since a
    /// relocation on this machine counts from where the instruction starts, which is where the
    /// hole is.
    fn field(&mut self, fixup: &Fixup, field: aarch64::Fixup) -> Result<(), Trouble> {
        let line = fixup.line;
        let bad = |why: String| Trouble { line, why };
        let residue = self.reduce_kept(&fixup.sum, fixup.jump).map_err(bad)?;
        let (name, addend) = match residue.left.as_slice() {
            [] if relative(field) => {
                let at = fixup.at as usize;
                let bytes = &mut self.parts[fixup.part].bytes[at..at + 4];
                let word = u32::from_le_bytes(bytes.try_into().expect("four bytes"));
                let word = field.apply(word, residue.constant).ok_or_else(|| {
                    bad(format!("{} is out of the reach of {}", residue.constant, field.name()))
                })?;
                bytes.copy_from_slice(&word.to_le_bytes());
                return Ok(());
            }
            [Left { coeff: 1, what: What::Symbol(name), .. }] if !relative(field) => {
                (name.clone(), residue.constant)
            }
            [
                Left { coeff: 1, what: What::Symbol(name), .. },
                Left { coeff: -1, at: Some((part, offset)), .. },
            ]
            | [
                Left { coeff: -1, at: Some((part, offset)), .. },
                Left { coeff: 1, what: What::Symbol(name), .. },
            ] if relative(field) && *part == fixup.part => {
                (name.clone(), residue.constant + fixup.at as i64 - offset)
            }
            _ => {
                return Err(bad(format!(
                    "an expression that {} cannot say, which is a name and a number added to it",
                    field.name()
                )));
            }
        };
        if let Some(&sym) = self.known.get(&name) {
            if self.syms[sym].numbered && self.syms[sym].at != Held::Undefined {
                self.relocated.insert(sym);
            } else if self.syms[sym].numbered {
                let number = name.split('\u{1}').next().unwrap_or(&name);
                return Err(bad(format!(
                    "'{number}f' goes on to a '{number}:' and there is none below it"
                )));
            }
        }
        self.parts[fixup.part].relocs.push(Reloc {
            at: fixup.at as usize,
            symbol: name,
            kind: Reference::Field(field),
            addend,
            after: 0,
        });
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
        // An ifunc is kept whatever its binding and whatever the reference is, because the place
        // the name marks is its resolver and the distance to that is not where a call to it goes.
        // gas leaves every reference to one to the linker, which makes a stub that goes through
        // the slot the resolver's answer is put in.
        let kept = self.known.get(name).is_some_and(|&sym| {
            (self.syms[sym].sort == Sort::Ifunc
                || self.syms[sym].binding == Binding::Weak
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

impl What {
    /// Whether this is that name.
    fn is(&self, name: &str) -> bool {
        matches!(self, What::Symbol(named) if named == name)
    }
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

/// The value of an expression that names nothing, or nothing for one that does or does not parse.
///
/// For the instruction reader, whose displacements are written with the same arithmetic a directive
/// is. busybox's SHA code writes `80+0*16(%rdi)` so that the offsets line up with the rounds, and
/// its TLS code writes `1*8(%r12)` for the words of a number.
pub(crate) fn constant(text: &str) -> Option<i64> {
    let mut parser = Parser {
        text: text.trim(),
        at: 0,
        here: (0, 0),
        values: None,
        reader: None,
        guessed: None,
    };
    parser.whole().ok()?.flat()
}

/// The directives that write data, after which gas pads code differently. See
/// [`Reader::after_data`].
const DATA: &[&str] = &[
    "byte", "short", "word", "hword", "value", "2byte", "long", "int", "4byte", "quad", "8byte",
    "xword", "dword", "octa", "uleb128", "sleb128", "ascii", "asciz", "string", "incbin", "space",
    "skip", "zero", "fill",
];

/// One expression, being read.
struct Parser<'a> {
    text: &'a str,
    at: usize,
    here: (usize, i64),
    /// The names set to a number so far, which are that number wherever they are written. See
    /// [`Reader::values`].
    values: Option<&'a Map<String, i64>>,
    /// The file the expression is in, which is where a label has a place for an operator that wants
    /// a number to ask about. Nothing for a displacement read on its own, which has no file around
    /// it and so only takes numbers.
    reader: Option<&'a Reader>,
    /// The places this expression took from the pass before rather than from this one, for the
    /// file to check once it has been laid out. See [`Reader::guesses`].
    guessed: Option<&'a mut Vec<(String, Held)>>,
}

impl Parser<'_> {
    /// The whole of it, and nothing left over.
    fn whole(&mut self) -> Result<Sum, String> {
        let sum = self.logical()?;
        self.space();
        if self.at < self.text.len() {
            return Err(format!(
                "'{}' is left over at the end of an expression",
                &self.text[self.at..]
            ));
        }
        Ok(sum)
    }

    /// `&&` and `||`, which bind loosest of all and come out as one or nothing, the way gas has
    /// them. `||` is the looser of the two, as it is in C.
    fn logical(&mut self) -> Result<Sum, String> {
        let mut left = self.both()?;
        loop {
            self.space();
            if !self.eat("||") {
                return Ok(left);
            }
            let right = self.both()?;
            let (a, b) = (self.number(left, "||")?, self.number(right, "||")?);
            left = Sum::just(i64::from(a != 0 || b != 0));
        }
    }

    /// `&&`, one step tighter than `||`.
    fn both(&mut self) -> Result<Sum, String> {
        let mut left = self.comparison()?;
        loop {
            self.space();
            if !self.eat("&&") {
                return Ok(left);
            }
            let right = self.comparison()?;
            let (a, b) = (self.number(left, "&&")?, self.number(right, "&&")?);
            left = Sum::just(i64::from(a != 0 && b != 0));
        }
    }

    /// The six comparisons, which bind looser than addition and come out as minus one when they
    /// hold and nothing when they do not. That is gas's truth, and it is what the kernel's
    /// `.skip -((new - old) > 0) * (new - old)` counts on: the minus in front turns it back into
    /// one.
    ///
    /// Two sides that name labels are compared by their difference when neither is a number on its
    /// own, which is what `(1f - 0f) == 5` needs when both labels are in one section.
    fn comparison(&mut self) -> Result<Sum, String> {
        let mut left = self.sum()?;
        loop {
            self.space();
            let Some(op) = self.one_of(&["==", "!=", "<>", "<=", ">=", "<", ">"]) else {
                return Ok(left);
            };
            let right = self.sum()?;
            let order = match (self.settle(&left), self.settle(&right)) {
                (Some(a), Some(b)) => a.cmp(&b),
                _ => {
                    let apart = left.clone().plus(right.clone().minus());
                    self.number(apart, op)?.cmp(&0)
                }
            };
            let holds = match op {
                "==" => order.is_eq(),
                "!=" | "<>" => order.is_ne(),
                "<=" => order.is_le(),
                ">=" => order.is_ge(),
                "<" => order.is_lt(),
                _ => order.is_gt(),
            };
            left = Sum::just(if holds { -1 } else { 0 });
        }
    }

    /// Addition and subtraction, which are the two that keep working when names are involved.
    fn sum(&mut self) -> Result<Sum, String> {
        let mut left = self.bitwise()?;
        loop {
            self.space();
            let Some(op) = self.one_of(&["+", "-"]) else { return Ok(left) };
            let right = self.bitwise()?;
            left = if op == "+" { left.plus(right) } else { left.plus(right.minus()) };
        }
    }

    /// The bitwise operators, which gas binds tighter than addition and looser than
    /// multiplication, unlike C. `!` between two numbers is the first or'd with the complement of
    /// the second.
    fn bitwise(&mut self) -> Result<Sum, String> {
        let mut left = self.product()?;
        loop {
            self.space();
            // Not the first half of `||`, `&&` or `!=`, which belong to the levels above.
            let rest = &self.text[self.at..];
            if rest.starts_with("||") || rest.starts_with("&&") || rest.starts_with("!=") {
                return Ok(left);
            }
            let Some(op) = self.one_of(&["|", "^", "&", "!"]) else { return Ok(left) };
            let right = self.product()?;
            left = self.arithmetic(left, right, op)?;
        }
    }

    /// Multiplication, division, remainder and the two shifts, which are gas's tightest binary
    /// operators.
    fn product(&mut self) -> Result<Sum, String> {
        let mut left = self.unary()?;
        loop {
            self.space();
            let Some(op) = self.one_of(&["*", "/", "%", "<<", ">>"]) else { return Ok(left) };
            let right = self.unary()?;
            // A name times a number is still a name counted that many times, which is worth keeping
            // because `foo*2 - foo` is a thing a macro produces. Everything else here wants two
            // numbers, and labels whose distance apart is known are one.
            left = match (op, left.flat(), right.flat()) {
                ("*", _, Some(factor)) => left.times(factor),
                ("*", Some(factor), _) => right.times(factor),
                _ => self.arithmetic(left, right, op)?,
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
            return Ok(Sum::just(!self.number(inner, "~")?));
        }
        if self.eat("!") {
            let inner = self.unary()?;
            return Ok(Sum::just(i64::from(self.number(inner, "!")? == 0)));
        }
        self.primary()
    }

    /// What one side of an operator that wants a number comes to, or a message naming the
    /// operator when it names something that is not a number here.
    fn number(&mut self, sum: Sum, op: &str) -> Result<i64, String> {
        self.settle(&sum).ok_or_else(|| format!("'{op}' of something that names a symbol"))
    }

    /// The number a sum comes to, when it is one.
    ///
    /// Either it names nothing, or every label in it is in the file and they cancel section by
    /// section, which is a distance between places and so a number however the sections end up
    /// being placed. A label further down the file has no place yet on this pass, so the place it
    /// had on the last one is used, and the file checks at the end that the guess held. See
    /// [`Reader::guesses`].
    fn settle(&mut self, sum: &Sum) -> Option<i64> {
        if let Some(value) = sum.flat() {
            return Some(value);
        }
        let reader = self.reader?;
        let guessed = self.guessed.as_deref_mut()?;
        let (constant, net) = reader.evaluate(sum, true, guessed, 0)?;
        net.values().all(|coeff| *coeff == 0).then_some(constant)
    }

    /// A number, a name, a character, `.`, or the whole thing again in brackets.
    fn primary(&mut self) -> Result<Sum, String> {
        self.space();
        let rest = &self.text[self.at..];
        if rest.is_empty() {
            return Err("an expression that stops before it says anything".to_owned());
        }
        // gas takes square brackets in an expression the way it takes round ones, and gcc's i386
        // code writes `_GLOBAL_OFFSET_TABLE_+[.-.L1]` with them.
        for (open, close) in [("(", ")"), ("[", "]")] {
            if self.eat(open) {
                let inner = self.logical()?;
                self.space();
                if !self.eat(close) {
                    return Err("a bracket that was opened and never closed".to_owned());
                }
                return Ok(inner);
            }
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
            if let Some(&value) = self.values.and_then(|values| values.get(&name)) {
                return Ok(Sum::just(value));
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
        } else if rest.starts_with('0') && rest.as_bytes().get(1).is_some_and(u8::is_ascii_digit) {
            // Octal only when a digit follows, because a nought on its own is a number too and
            // `0*16` is nought times sixteen rather than an octal number with no digits in it.
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
    fn arithmetic(&mut self, left: Sum, right: Sum, op: &str) -> Result<Sum, String> {
        let a = self.number(left, op)?;
        let b = self.number(right, op)?;
        Ok(Sum::just(self.arithmetic_number(a, b, op)?))
    }

    /// The same, once both are numbers.
    ///
    /// A shift right is of the bits and not of the signed number, which is how gas does it: `-1 >>
    /// 60` is fifteen.
    fn arithmetic_number(&self, a: i64, b: i64, op: &str) -> Result<i64, String> {
        Ok(match op {
            "|" => a | b,
            "^" => a ^ b,
            "&" => a & b,
            "!" => a | !b,
            "<<" => a.wrapping_shl(shift(b)?),
            ">>" => (a as u64).wrapping_shr(shift(b)?) as i64,
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
/// `nop` on AArch64, which is what the padding in front of an instruction is made of there.
const A64_NOP: u32 = 0xd503_201f;

/// Whether an AArch64 field is a distance from the instruction it is in, which is the one kind a
/// file can work out on its own.
fn relative(field: aarch64::Fixup) -> bool {
    use aarch64::Fixup;
    matches!(
        field,
        Fixup::Jump26
            | Fixup::Call26
            | Fixup::CondBr19
            | Fixup::TestBr14
            | Fixup::Literal19
            | Fixup::AdrLo21
    )
}

/// The number DWARF gives an AArch64 register, which a frame rule may name either way: `x19` is
/// nineteen, the stack pointer is thirty one and the vector registers start at sixty four.
fn aarch64_dwarf(text: &str) -> Option<u16> {
    if let Ok(number) = text.parse::<u16>() {
        return Some(number);
    }
    let lower = text.to_ascii_lowercase();
    match lower.as_str() {
        "sp" => return Some(31),
        "fp" => return Some(29),
        "lr" => return Some(30),
        _ => {}
    }
    let (first, number) = lower.split_at(1);
    let number = number.parse::<u16>().ok().filter(|&number| number < 32)?;
    match first {
        "x" | "w" if number < 31 => Some(number),
        "v" | "q" | "d" | "s" => Some(64 + number),
        _ => None,
    }
}

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
///
/// gas takes spaces between the name and the colon too, and the kernel's entry_64.S writes `0 :`.
/// The length that comes back with the name is how much of the text the label took, colon and all.
fn labelled(text: &str) -> Option<(String, usize)> {
    let bytes = text.as_bytes();
    if bytes.is_empty() || !(starts(bytes[0]) || bytes[0].is_ascii_digit()) {
        return None;
    }
    let end = text.find(|ch: char| !carries_on(ch as u8))?;
    let colon = end + text[end..].find(|ch: char| ch != ' ' && ch != '\t')?;
    // Not `::`, which is a different thing in gas, and not a bare name with nothing after it.
    if bytes.get(colon) != Some(&b':') || bytes.get(colon + 1) == Some(&b':') {
        return None;
    }
    Some((text[..end].to_owned(), colon + 1))
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

/// Which relocation a read of a slot of the global offset table asks for on i386, from the bytes of
/// the instruction it is in.
///
/// The instructions are the ones [`crate::bytes::slot`] names for x86-64. gas asks for
/// `R_386_GOT32X` in them only when the address is four bytes of displacement after a base, or the
/// displacement alone, since those are the shapes the linker knows how to rewrite, and asks for
/// `R_386_GOT32` everywhere else: `foo@GOT(,%ecx,4)`, a store, `lea` and `push` among them.
fn slot_i386(bytes: &[u8]) -> Reference {
    if crate::bytes::slot(bytes, Mode::Bits32) != Reference::GotBare {
        return Reference::SlotKept;
    }
    let prefixes = bytes
        .iter()
        .take_while(|byte| {
            matches!(byte, 0x26 | 0x2E | 0x36 | 0x3E | 0x64 | 0x65 | 0x67 | 0xF0 | 0xF2 | 0xF3)
        })
        .count();
    // Every instruction [`crate::bytes::slot`] lets through has an opcode of one byte, so the
    // addressing byte is the one after it.
    let Some(&modrm) = bytes.get(prefixes + 1) else { return Reference::SlotKept };
    let (mode, rm) = (modrm >> 6, modrm & 7);
    if mode == 2 || (mode == 0 && rm == 5) { Reference::Slot } else { Reference::SlotKept }
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

/// The quoted strings `arg` is made of when it is nothing but quoted strings with blanks between,
/// each with its quotes, or `arg` itself when it is anything else.
fn adjacent(arg: &str) -> Vec<&str> {
    let mut pieces = Vec::new();
    let mut rest = arg;
    while let Some(after) = rest.strip_prefix('"') {
        let mut escaped = false;
        let Some(end) = after.char_indices().find_map(|(at, c)| {
            let closes = c == '"' && !escaped;
            escaped = c == '\\' && !escaped;
            closes.then_some(at)
        }) else {
            return vec![arg];
        };
        pieces.push(&rest[..end + 2]);
        rest = after[end + 1..].trim_start();
    }
    if pieces.is_empty() || !rest.is_empty() {
        return vec![arg];
    }
    pieces
}

/// The words of a directive split at spaces, with a quoted string kept whole however many spaces
/// are in it, quotes and all.
fn words(text: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut chars = text.trim().chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() || c == ',' {
            chars.next();
            continue;
        }
        let mut word = String::new();
        if c == '"' {
            word.push(c);
            chars.next();
            loop {
                match chars.next() {
                    Some('\\') => {
                        word.push('\\');
                        word.extend(chars.next());
                    }
                    Some('"') => {
                        word.push('"');
                        break;
                    }
                    Some(c) => word.push(c),
                    None => return Err("a string that is never closed".to_owned()),
                }
            }
        } else {
            while let Some(c) = chars.next_if(|c| !c.is_whitespace() && *c != '"') {
                word.push(c);
            }
        }
        words.push(word);
    }
    Ok(words)
}

/// The text with its quotes taken off, if it had any.
fn unquoted(text: &str) -> String {
    text.strip_prefix('"').and_then(|rest| rest.strip_suffix('"')).unwrap_or(text).to_owned()
}

/// Split on a separator that is outside every string and every bracket.
///
/// A number of up to a hundred and twenty eight bits, in any base gas reads, with a minus sign
/// taken as two's complement.
fn wide(text: &str) -> Option<u128> {
    let text = text.trim();
    let (negative, text) = match text.strip_prefix('-') {
        Some(rest) => (true, rest.trim_start()),
        None => (false, text),
    };
    let lower = text.to_ascii_lowercase();
    let (digits, radix) = if let Some(rest) = lower.strip_prefix("0x") {
        (rest, 16)
    } else if let Some(rest) = lower.strip_prefix("0b") {
        (rest, 2)
    } else if lower.len() > 1 && lower.starts_with('0') {
        (&lower[1..], 8)
    } else {
        (lower.as_str(), 10)
    };
    if digits.is_empty() {
        return None;
    }
    let value = u128::from_str_radix(digits, radix).ok()?;
    Some(if negative { value.wrapping_neg() } else { value })
}

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
            // A character constant, which is one character whatever it is, so `','` is not the
            // end of an operand. Its closing quote is optional, as it is to gas.
            '\'' => {
                piece.push(ch);
                let mut next = chars.next();
                if next == Some('\\') {
                    piece.push('\\');
                    next = chars.next();
                }
                piece.extend(next);
                if chars.clone().next() == Some('\'') {
                    piece.push('\'');
                    chars.next();
                }
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

/// The byte a prefix written as a word is, for the ones a line may write in front of an instruction.
///
/// `rex64` is not one of them, since a REX byte has to be merged with the one the instruction may
/// already have rather than put in front of it, and nothing a kernel writes asks for that. It is
/// still a line of its own, which the encoder has a row for.
fn prefix(word: &str) -> Option<u8> {
    Some(match word {
        "es" => 0x26,
        "cs" => 0x2E,
        "ss" => 0x36,
        "ds" => 0x3E,
        "fs" => 0x64,
        "gs" => 0x65,
        "addr32" => 0x67,
        "data16" => 0x66,
        "rep" | "repe" | "repz" | "xrelease" => 0xF3,
        "repne" | "repnz" | "xacquire" => 0xF2,
        "lock" => 0xF0,
        _ => return None,
    })
}

/// Where a prefix goes among the others, in the order gas writes them.
///
/// A segment first, then the address size, then the operand size, then a repeat, then `lock`, and
/// the REX byte last of all, since it has to be right in front of the opcode. Two of the same rank
/// are two prefixes that say the same thing, which is a line gas refuses too.
fn rank(byte: u8) -> Option<u8> {
    match byte {
        0x26 | 0x2E | 0x36 | 0x3E | 0x64 | 0x65 => Some(1),
        0x67 => Some(2),
        0x66 => Some(3),
        0xF2 | 0xF3 => Some(4),
        0xF0 => Some(5),
        0x40..=0x4F => Some(6),
        _ => None,
    }
}

/// An instruction with one more prefix in front of it, put among the prefixes it already has where
/// gas would put it, so that `lock incl %gs:(%rax)` comes out `65 f0 ff 00` the way gas writes it.
///
/// Every place in the instruction that names something is after the prefixes, so each of them moves
/// along by the byte.
pub(crate) fn prefixed(written: &mut crate::instruction::Written, byte: u8) -> Result<(), String> {
    let mine = rank(byte).unwrap_or(0);
    let mut at = 0;
    while let Some(theirs) = written.bytes.get(at).and_then(|&had| rank(had)) {
        if theirs == mine && theirs != 6 {
            return Err(format!(
                "a prefix of the same kind as {byte:#04x} is already on the instruction"
            ));
        }
        if theirs > mine {
            break;
        }
        at += 1;
    }
    written.bytes.insert(at, byte);
    for hole in &mut written.holes {
        hole.at += 1;
    }
    Ok(())
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
    let string = next.len() > 1 && next.ends_with(['b', 'w', 'l', 'q']);
    let which = if string { &next[..next.len() - 1] } else { "" };
    let prefix = match (unequal, which) {
        (false, "movs" | "stos" | "lods" | "ins" | "outs") => "rep",
        (false, "scas" | "cmps") => "repe",
        (true, "scas" | "cmps") => "repne",
        _ => return None,
    };
    Some((format!("{prefix} {next}"), after))
}

#[cfg(test)]
mod tests {
    use super::*;

    use rucc_object::{Reference, Tls};

    /// The file, read, with a failure reported as a panic naming the line it was on.
    fn assembled(text: &str) -> Assembled {
        match read(text, Arch::X86_64) {
            Ok(assembled) => assembled,
            Err(trouble) => panic!("line {}: {}", trouble.line, trouble.why),
        }
    }

    /// `.symver` as gas 2.42 writes it. `@` and `@@` are a copy at the same place that keeps the
    /// type, size and binding, `@@@` renames the name it is about, and so does any spelling of a
    /// name the file only refers to, with the relocations against it following the new name.
    #[test]
    fn a_symver_is_a_copy_or_a_rename_as_in_gas() {
        let file = assembled(concat!(
            ".text\n.globl real\n.type real, @function\nreal: ret\n.size real, .-real\n",
            ".symver real, real@VERS_1\n.symver real, real_d@@VERS_2\n.symver real, real_e@\n",
            ".globl ren\n.type ren, @function\nren: ret\n.symver ren, ren@@@VERS_3\n",
            ".symver ext, ext@GLIBC_2.2.5\ncall ext\ncall ren\n.symver nothing, nothing@V\n",
        ));
        let names: Vec<&str> = file.names.iter().map(|name| name.name.as_str()).collect();
        assert_eq!(
            names,
            ["real", "ren@@VERS_3", "ext@GLIBC_2.2.5", "real@VERS_1", "real_d@@VERS_2", "real_e@"]
        );
        let real = &file.names[0];
        for copy in &file.names[3..] {
            assert_eq!((copy.at, copy.size, copy.sort), (real.at, real.size, real.sort));
            assert_eq!(copy.binding, real.binding);
        }
        let called: Vec<&str> =
            file.parts[0].relocs.iter().map(|reloc| reloc.symbol.as_str()).collect();
        assert_eq!(called, ["ext@GLIBC_2.2.5", "ren@@VERS_3"]);

        let twice = read(".text\na: b:\n.symver a, x@V\n.symver b, x@V\n", Arch::X86_64);
        assert!(twice.unwrap_err().why.contains("defined twice"));
        let bare = read(".text\na:\n.symver a, x\n", Arch::X86_64);
        assert!(bare.unwrap_err().why.contains("no '@'"));
    }

    /// gcc's `-g` output leaves the line table to the assembler: `.file` and `.loc` say where each
    /// instruction came from, and the table goes in the empty `.debug_line` gcc names. The bytes
    /// are the ones gas 2.44 writes for the same file, view numbers and all.
    #[test]
    fn file_and_loc_make_the_line_table_gas_makes() {
        let file = assembled(concat!(
            ".file 0 \"/w\" \"/s/a.c\"\n.file 1 \"/s/a.c\"\n.text\nf:\n",
            ".loc 1 3 1 view -0\n.loc 1 4 5 view .LVU1\nnop\n.loc 1 20 2 is_stmt 0\nret\n",
            ".section .debug_info,\"\",@progbits\n.long 0\n.byte .LVU1\n",
            ".section .debug_line,\"\",@progbits\n",
        ));
        let part = |name: &str| file.parts.iter().find(|part| part.name == name).unwrap();
        #[rustfmt::skip]
        let table = [
            0x52, 0, 0, 0, 5, 0, 8, 0, 0x2e, 0, 0, 0, 1, 1, 1, 0xfb,
            0x0e, 0x0d, 0, 1, 1, 1, 1, 0, 0, 0, 1, 0, 0, 1, 1, 1,
            0x1f, 2, 0, 0, 0, 0, 0, 0, 0, 0, 2, 1, 0x1f, 2, 0x0f, 2,
            0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 5, 1, 0, 9, 2, 0,
            0, 0, 0, 0, 0, 0, 0, 0x14, 5, 5, 0x13, 5, 2, 6, 3, 0x10,
            0x20, 2, 1, 0, 1, 1,
        ];
        assert_eq!(part(".debug_line").bytes, table);
        assert_eq!(part(".debug_line_str").bytes, b"/w\0/s\0a.c\0a.c\0");
        assert_eq!(part(".debug_info").bytes, [0, 0, 0, 0, 1], "the second row is view one");
        let holes: Vec<usize> = part(".debug_line").relocs.iter().map(|reloc| reloc.at).collect();
        assert_eq!(holes, [0x22, 0x26, 0x30, 0x35, 0x3f]);

        let twice = ".file 1 \"a.c\"\n.file 1 \"b.c\"\n";
        assert!(read(twice, Arch::X86_64).unwrap_err().why.contains("already occupied"));
        let unknown = ".file 1 \"a.c\"\n.loc 1 1 frob\n";
        assert!(read(unknown, Arch::X86_64).unwrap_err().why.contains("frob"));
    }

    /// The shape of the kernel's la57toggle.S, which drops to thirty two bits in the middle of
    /// an x86-64 file, writes to a control register there and jumps back to a segment.
    #[test]
    fn a_mnemonic_is_read_whatever_its_case() {
        // The x86 selftests write `SYSENTER`, and gas takes a prefix the same way.
        let file = assembled(".text\nSYSENTER\nLOCK Incl (%rax)\nNop\n");
        assert_eq!(bytes(&file, ".text"), [0x0f, 0x34, 0xf0, 0xff, 0x00, 0x90]);
    }

    #[test]
    fn code32_reads_what_follows_in_thirty_two_bit_mode_until_code64() {
        let file = assembled(
            ".text\nmovq %cr0, %rax\n.code32\na: movl %cr0, %eax\nljmpl $0x10, $(b - a)\nb: incl %eax\n.code64\nincl %eax\n",
        );
        assert_eq!(
            bytes(&file, ".text"),
            [0x0f, 0x20, 0xc0, 0x0f, 0x20, 0xc0, 0xea, 0x0a, 0, 0, 0, 0x10, 0, 0x40, 0xff, 0xc0]
        );
        // The x86 selftests' thunks.S, which calls into thirty two bit code and jumps back with
        // the two operand `jmp` gas reads as a far one. The label is left to the linker.
        let file = assembled(".text\n.code32\ncall *%esi\njmp $0x33,$1f\n.code64\n1: ret\n");
        assert_eq!(bytes(&file, ".text"), [0xff, 0xd6, 0xea, 0, 0, 0, 0, 0x33, 0, 0xc3]);
        let long = read(".text\nljmpl $0x10, $0\n", Arch::X86_64).unwrap_err();
        assert!(long.why.contains("sixty four bit mode"), "{}", long.why);
        let narrow = read(".text\nmovl %cr0, %eax\n", Arch::X86_64).unwrap_err();
        assert!(narrow.why.contains("sixty four bit register"), "{}", narrow.why);
    }

    /// Three things the kernel's assembly writes that gas takes: `sgdtl` in thirty two bit mode, a
    /// push and a pop of two bytes, and the AVX spelling of the lane extracts and inserts narrower
    /// than a quadword. The bytes are the ones gas writes for the same lines.
    #[test]
    fn the_kernel_s_narrow_pushes_descriptor_stores_and_vex_lane_moves_assemble() {
        let file = assembled(
            ".text\n.code32\nsgdtl (%esp)\n.code64\npushw (%rax)\npushw $0xff\npushw $1\npopw %bx\n\
             vpextrd $1, %xmm9, (%r10)\nvpextrw $3, %xmm0, %eax\nvpinsrb $1, (%rax), %xmm1, %xmm12\n",
        );
        assert_eq!(
            bytes(&file, ".text"),
            [
                0x0f, 0x01, 0x04, 0x24, 0x66, 0xff, 0x30, 0x66, 0x68, 0xff, 0x00, 0x66, 0x6a, 0x01,
                0x66, 0x5b, 0xc4, 0x43, 0x79, 0x16, 0x0a, 0x01, 0xc5, 0xf9, 0xc5, 0xc0, 0x03, 0xc4,
                0x63, 0x71, 0x20, 0x20, 0x01,
            ]
        );
    }

    /// A listing that names a personality routine and a call site table, the way gcc writes a
    /// function with a `cleanup` under `-fexceptions`, gets a header that names the routine and
    /// says `zPLR`, a record that points at the table, and the table with its distances worked
    /// out. The length of the rows is a label further on, so it is written in four bytes.
    #[test]
    fn a_listing_that_names_a_personality_routine_keeps_it_and_its_call_site_table() {
        let text = "\t.text
\t.globl\tf
f:
\t.cfi_startproc
\t.cfi_personality 0x9b,DW.ref.__gcc_personality_v0
\t.cfi_lsda 0x1b,.LLSDA0
\tpushq\t%rbx
\t.cfi_def_cfa_offset 16
.LEHB0:
\tcall\tg
.LEHE0:
\tpopq\t%rbx
\tret
.L2:
\tmovq\t%rax, %rdi
\tcall\t_Unwind_Resume
\t.cfi_endproc
\t.section\t.gcc_except_table,\"a\",@progbits
.LLSDA0:
\t.byte\t0xff
\t.byte\t0xff
\t.byte\t0x1
\t.uleb128 .LLSDACSE0-.LLSDACSB0
.LLSDACSB0:
\t.uleb128 .LEHB0-f
\t.uleb128 .LEHE0-.LEHB0
\t.uleb128 .L2-f
\t.uleb128 0
.LLSDACSE0:
";
        let done = assembled(text);
        let table = done.parts.iter().find(|part| part.name == ".gcc_except_table").expect("one");
        // The push is one byte, the call five, and the pop and the return one each.
        assert_eq!(table.bytes, [0xff, 0xff, 0x01, 0x04, 1, 5, 8, 0]);
        let frame = done.parts.iter().find(|part| part.name == ".eh_frame").expect("one");
        assert!(frame.bytes.windows(5).any(|at| at == b"zPLR\0"), "{:x?}", frame.bytes);
        let named: Vec<&str> = frame.relocs.iter().map(|reloc| reloc.symbol.as_str()).collect();
        assert!(named.contains(&"DW.ref.__gcc_personality_v0"), "{named:?}");
        assert!(named.contains(&".LLSDA0"), "{named:?}");
    }

    /// A LEB128 number that counts to a label further on is written in as few bytes as it needs,
    /// as gas writes it, which is how the vDSO's `sigreturn.S` gets the length of each DWARF
    /// expression it writes by hand. The bytes are the ones gas 2.40 writes for the same lines.
    #[test]
    fn a_leb128_number_ahead_of_its_label_takes_the_bytes_it_needs() {
        let text = "\t.section .gcc_except_table,\"a\",@progbits
\t.byte 1
\t.uleb128 2f-1f
1:\t.uleb128 3
\t.byte 1
2:
\t.sleb128 1b-2b
\t.uleb128 4f-3f
3:\t.fill 200,1,0
4:
";
        let done = assembled(text);
        let table = done.parts.iter().find(|part| part.name == ".gcc_except_table").expect("one");
        assert_eq!(table.bytes[..7], [0x01, 0x02, 0x03, 0x01, 0x7e, 0xc8, 0x01]);
        assert_eq!(table.bytes.len(), 207);
    }

    /// A space between a `%` and its register is skipped, as gas skips it, which is how the
    /// kernel's `vmenter.S` reaches the assembler after the preprocessor. A `%` in an expression is
    /// still the remainder. The bytes are the ones GNU as writes for the same four lines.
    #[test]
    fn a_space_after_the_sigil_of_a_register_is_skipped() {
        let text = "\tcmpb $0, kvm_rebooting (% rip)\n\tmovq % rax, %rbx\n\
                    \tmovl $(10 % 3), %eax\n\tmovw % gs:8, %ax\n";
        let done = assembled(text);
        let code = done.parts.iter().find(|part| part.name == ".text").expect("one");
        assert_eq!(
            code.bytes,
            [
                0x80, 0x3d, 0, 0, 0, 0, 0, 0x48, 0x89, 0xc3, 0xb8, 1, 0, 0, 0, 0x65, 0x66, 0x8b,
                0x04, 0x25, 8, 0, 0, 0
            ]
        );
        assert_eq!(code.relocs[0].symbol, "kvm_rebooting");
    }

    /// A `.cfi_lsda` in an encoding the table has no way to write is refused, rather than passed
    /// over and the function's cleanups with it.
    #[test]
    fn a_call_site_table_in_an_encoding_the_table_cannot_write_is_refused() {
        let text = "f:\n\t.cfi_startproc\n\t.cfi_personality 0x9b,p\n\t.cfi_lsda 0x10,t\n\tret\n\t.cfi_endproc\n";
        let Err(trouble) = read(text, Arch::X86_64) else { panic!("read") };
        assert!(trouble.why.contains("encoding 0x10"), "{}", trouble.why);
    }

    /// The frame rules of a Mac listing make the same table as on ELF, in the section and with the
    /// flags clang gives it there, and each record names a place at the front of its function,
    /// which the object writer turns into the function's own name.
    #[test]
    fn a_mac_listing_with_frame_rules_has_an_unwind_table() {
        let text = "\t.section\t__TEXT,__text,regular,pure_instructions
\t.globl\t_f
_f:
\t.cfi_startproc
\tstp x29, x30, [sp, #-16]!
\t.cfi_def_cfa_offset 16
\tldp x29, x30, [sp], #16
\tret
\t.cfi_endproc
\t.subsections_via_symbols
";
        let done = match read_as(text, Arch::Aarch64, ObjectFormat::MachO) {
            Ok(done) => done,
            Err(trouble) => panic!("line {}: {}", trouble.line, trouble.why),
        };
        let table = done.parts.iter().find(|part| part.name == "__TEXT,__eh_frame").expect("one");
        // `S_COALESCED` in the type byte and `S_ATTR_LIVE_SUPPORT` among the attributes.
        assert_eq!(table.shape.mach & 0x0800_00ff, 0x0800_000b);
        let rows = [0x44, 0x0e, 0x10];
        assert!(table.bytes.windows(rows.len()).any(|at| at == rows), "{:x?}", table.bytes);
        let [reloc] = table.relocs.as_slice() else { panic!("one record, one relocation") };
        assert_eq!(reloc.kind, Reference::Data);
        let at = |wanted: &str| done.names.iter().find(|name| name.name == wanted).unwrap().at;
        assert_eq!(at(&reloc.symbol), at("_f"));
    }

    #[test]
    fn a_mac_listing_reads_as_segments_and_sections() {
        let text = "\t.section\t__TEXT,__text,regular,pure_instructions
\t.p2align\t2
\t.globl\t_main
\t.private_extern\t_main
_main:
Lmain_0:
\tadrp x0, _counter@PAGE
\tldr w0, [x0, _counter@PAGEOFF]
\tret
\t.section\t__DATA,__data
\t.globl\t_counter
_counter:
\t.long\t3
\t.globl\t_zeros
\t.zerofill\t__DATA,__bss,_zeros,400,4
\t.long\t4
\t.section\t__DATA,__thread_data,thread_local_regular
_tls$tlv$init:
\t.long\t5
\t.weak_definition\t_counter
\t.subsections_via_symbols
";
        let done = match read_as(text, Arch::Aarch64, ObjectFormat::MachO) {
            Ok(done) => done,
            Err(trouble) => panic!("line {}: {}", trouble.line, trouble.why),
        };
        assert!(done.subsections);
        let names: Vec<&str> = done.parts.iter().map(|part| part.name.as_str()).collect();
        assert_eq!(
            names,
            ["__TEXT,__text", "__DATA,__data", "__DATA,__bss", "__DATA,__thread_data"]
        );
        assert!(done.parts[0].shape.exec && done.parts[3].shape.thread);
        // `.zerofill` made room without going there, so the second word is in `__data`.
        assert_eq!(done.parts[1].bytes, [3, 0, 0, 0, 4, 0, 0, 0]);
        assert_eq!((done.parts[2].size, done.parts[2].align), (400, 16));
        let named = |name: &str| done.names.iter().find(|n| n.name == name).unwrap();
        assert_eq!(named("_main").visibility, Visibility::Hidden);
        assert_eq!(named("_counter").binding, Binding::Weak);
        assert_eq!(named("_zeros").at, Held::In { part: 2, offset: 0 });
        assert_eq!(done.parts[0].relocs.len(), 2);
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
        read(text, Arch::X86_64)
            .err()
            .unwrap_or_else(|| panic!("this was read and should not have been"))
    }

    /// A file for AArch64, read.
    fn aarch64(text: &str) -> Assembled {
        match read(text, Arch::Aarch64) {
            Ok(assembled) => assembled,
            Err(trouble) => panic!("line {}: {}", trouble.line, trouble.why),
        }
    }

    /// The words of a section.
    fn words(assembled: &Assembled, name: &str) -> Vec<u32> {
        let bytes = bytes(assembled, name);
        bytes.chunks(4).map(|word| u32::from_le_bytes(word.try_into().unwrap())).collect()
    }

    #[test]
    fn align_on_aarch64_is_a_power_of_two_and_on_x86_64_a_count_of_bytes() {
        // What gcc writes in front of an eight byte variable on each machine.
        let read = aarch64("\t.data\n\t.byte 1\n\t.align 3\n\t.byte 2\n");
        assert_eq!(bytes(&read, ".data"), [1, 0, 0, 0, 0, 0, 0, 0, 2]);
        let read = assembled("\t.data\n\t.byte 1\n\t.align 4\n\t.byte 2\n");
        assert_eq!(bytes(&read, ".data"), [1, 0, 0, 0, 2]);
    }

    #[test]
    fn an_aarch64_branch_in_the_file_is_filled_in_and_a_name_is_left_to_the_linker() {
        let read = aarch64(concat!(
            "# 1 \"f.s\"\n",
            "f:\tcbz x0, 1f // to the return\n",
            "\tbl g\n",
            "\tadrp x1, table+8\n",
            "\tadd x1, x1, :lo12:table+8\n",
            "1:\tret\n",
            "\t.p2align 3\n",
            "\t.word 5\n",
        ));
        // `cbz` reaches four words on, the padding is one `nop`, and `.word` is four bytes here.
        assert_eq!(
            words(&read, ".text"),
            [0xb400_0080, 0x9400_0000, 0x9000_0001, 0x9100_0021, 0xd65f_03c0, 0xd503_201f, 5]
        );
        let text = read.parts.iter().find(|part| part.name == ".text").unwrap();
        let relocs: Vec<_> =
            text.relocs.iter().map(|r| (r.at, r.symbol.as_str(), r.kind, r.addend)).collect();
        assert_eq!(
            relocs,
            [
                (4, "g", Reference::Field(aarch64::Fixup::Call26), 0),
                (8, "table", Reference::Field(aarch64::Fixup::AdrPage21), 8),
                (12, "table", Reference::Field(aarch64::Fixup::AddLo12), 8),
            ]
        );
    }

    #[test]
    fn an_aarch64_frame_starts_at_the_stack_pointer_with_the_return_address_in_x30() {
        let read = aarch64(concat!(
            "f:\n\t.cfi_startproc\n\tstr x19, [sp, #-16]!\n\t.cfi_def_cfa_offset 16\n",
            "\t.cfi_offset x19, -16\n\tldr x19, [sp], #16\n\t.cfi_restore 19\n",
            "\t.cfi_def_cfa_offset 0\n\tret\n\t.cfi_endproc\n",
        ));
        let frame = bytes(&read, ".eh_frame");
        // The two alignments, the return address column, the augmentation, and then the header's
        // rules: the frame is at the stack pointer, 31, plus nothing, and there is no rule for x30
        // because the call left it in the register rather than on the stack.
        assert_eq!(&frame[12..20], &[1, 0x78, 30, 1, 0x1b, 0x0c, 31, 0]);
        assert_eq!(&frame[20..24], &[0; 4]);
        // x19 saved two slots below the end of the frame, after the first instruction.
        assert!(frame.windows(5).any(|w| w == [0x44, 0x0e, 16, 0x93, 2]), "{frame:x?}");
    }

    #[test]
    fn an_aarch64_return_address_signed_in_the_prologue_says_so_in_the_unwind_table() {
        // What clang writes for a function under -mbranch-protection=pac-ret+bti, and the kernel's
        // `bti c` and `hint #34` for the landing pad a function written in assembly begins with.
        let read = aarch64(concat!(
            "f:\n\t.cfi_startproc\n\tpaciasp\n\t.cfi_negate_ra_state\n",
            "\tstp x29, x30, [sp, #-16]!\n\t.cfi_def_cfa_offset 16\n",
            "\tldp x29, x30, [sp], #16\n\tautiasp\n\tret\n\t.cfi_endproc\n",
            "g:\n\tbti c\n\thint #34\n\tbti j\n\tret\n",
        ));
        assert_eq!(
            words(&read, ".text"),
            [
                0xd503_233f,
                0xa9bf_7bfd,
                0xa8c1_7bfd,
                0xd503_23bf,
                0xd65f_03c0,
                0xd503_245f,
                0xd503_245f,
                0xd503_249f,
                0xd65f_03c0
            ]
        );
        // The flip after the first instruction, and then the frame moving after the second.
        let frame = bytes(&read, ".eh_frame");
        assert!(frame.windows(5).any(|w| w == [0x44, 0x2d, 0x44, 0x0e, 16]), "{frame:x?}");
        let trouble = super::read("f:\n\t.cfi_startproc\n\t.cfi_b_key_frame\n", Arch::Aarch64);
        assert!(trouble.unwrap_err().why.contains("B key"));
    }

    #[test]
    fn an_aarch64_register_named_with_req_is_the_register_until_unreq() {
        // How the arm64 kernel's `assembler.h` names its registers, and a name given again after
        // `.unreq`. The words are what llvm-mc writes for the same lines.
        let read = aarch64(concat!(
            "\t.irp n,0,1,2\nwx\\n .req w\\n\n\t.endr\n",
            "lr .req x30\ntmp .req x9\n",
            "\tmov wx1, wx2\n\tadd x0, tmp, #8\n\tstp x29, lr, [sp, #-16]!\n\tldr wx0, [tmp, #4]\n",
            "\t.unreq tmp\ntmp .req x10\n\tmov tmp, lr\n",
        ));
        assert_eq!(
            words(&read, ".text"),
            [0x2a02_03e1, 0x9100_2120, 0xa9bf_7bfd, 0xb940_0520, 0xaa1e_03ea]
        );
    }

    #[test]
    fn an_aarch64_immediate_is_worked_out_before_it_is_read() {
        // What the arm64 kernel's assembly is once the preprocessor has been over it, and the
        // `bti` macro it falls back on for an assembler without the instruction. The words are
        // what llvm-mc writes for `mov x0, #192`, `add x1, x1, #8`, `mov x2, #5`, `ldr x3, [x4,
        // #16]`, `stp x29, x30, [sp, #-32]!` and `bti c`.
        let read = aarch64(concat!(
            "\t.equ five, 5\n\t.set back, -32\n",
            "\t.macro bti, targets\n\t.equ .L__bti_targets_c, 34\n",
            "\thint #.L__bti_targets_\\targets\n\t.endm\n",
            "\tmov x0, #((0x00000040) | (0x00000080))\n\tadd x1, x1, #(1 << 3)\n",
            "\tmov x2, #five\n\tldr x3, [x4, #(8*2)]\n\tstp x29, x30, [sp, #back]!\n\tbti c\n",
        ));
        assert_eq!(
            words(&read, ".text"),
            [0xd280_1800, 0x9100_2021, 0xd280_00a2, 0xf940_0883, 0xa9be_7bfd, 0xd503_245f]
        );
    }

    #[test]
    fn an_aarch64_load_of_a_literal_is_a_move_or_a_word_in_the_pool() {
        // What the arm64 kernel's head.S and the hypervisor's vectors write, and the words llvm-mc
        // writes for the same text: a number a `mov` takes is the `mov`, and anything else is a
        // word in the pool `.ltorg` puts down, or the one at the end of the section.
        let read = aarch64(concat!(
            "\tldr x0, =0x1234\n\tldr x1, =0x123456789abcdef0\n\tldr w2, =sym\n\tb 1f\n",
            "\t.ltorg\n1:\t.inst(0xd503201f)\n\t.inst 0xd503233f\n",
            "k0 .req v0\n\tdup k0.4s, w6\n\tadd x3, x3, 128 + 16\n\tldr x4, =0x1122334455667788\n",
        ));
        assert_eq!(
            words(&read, ".text"),
            [
                0xd282_4680,
                0x5800_0061,
                0x1800_0082,
                0x1400_0004,
                0x9abc_def0,
                0x1234_5678,
                0,
                0xd503_201f,
                0xd503_233f,
                0x4e04_0cc0,
                0x9102_4063,
                0x5800_0024,
                0x5566_7788,
                0x1122_3344
            ]
        );
        assert_eq!(relocs(&read, ".text"), [(24, "sym", Reference::Address { bytes: 4 }, 0)]);
    }

    #[test]
    fn an_aarch64_sum_after_a_relocation_specifier_may_have_spaces_in_it() {
        // What the arm64 kernel's `adr_l` writes for `aes_enc_tab + 1`, under the `.cpu` line
        // crc32-core.S starts with, and the relocations llvm-mc writes for the same text.
        let read = aarch64(concat!(
            "\t.cpu generic+crc\n\tadrp x0, table + 1\n\tadd x0, x0, :lo12:table + 1\n",
            "\tldr x1, [x0, #:lo12:table + 8]\n",
        ));
        let relocs: Vec<_> =
            relocs(&read, ".text").into_iter().map(|(at, _, _, addend)| (at, addend)).collect();
        assert_eq!(relocs, [(0, 1), (4, 1), (8, 8)]);
    }

    #[test]
    fn a_wide_move_of_part_of_a_number_is_the_plain_move() {
        // The arm64 kernel's `mov_q`, cut down to the lines each constant takes. The words are
        // what llvm-mc writes for the same text.
        let read = aarch64(concat!(
            "\tmovz x0, :abs_g3:0x3320646e61707865\n\tmovk x0, :abs_g2_nc:0x3320646e61707865\n",
            "\tmovk x0, :abs_g1_nc:0x3320646e61707865\n\tmovk x0, :abs_g0_nc:0x3320646e61707865\n",
            "\tmovz x1, :abs_g1_s:0xffffffff80000000\n\tmovk x1, :abs_g0_nc:0xffffffff80000000\n",
            "\tmovz x2, :abs_g2_s:0x123456789a\n\tmovz x3, #:abs_g1_s:0x1234\n",
        ));
        assert_eq!(
            words(&read, ".text"),
            [
                0xd2e6_6400,
                0xf2cc_8dc0,
                0xf2ac_2e00,
                0xf28f_0ca0,
                0x92af_ffe1,
                0xf280_0001,
                0xd2c0_0242,
                0xd2a0_0003
            ]
        );
        // A number too wide for a checked group is refused, as GNU as refuses it.
        assert!(super::read("\tmovz x0, :abs_g0:0x10000\n", Arch::Aarch64).is_err());
    }

    #[test]
    fn a_wide_move_of_a_name_set_to_labels_is_worked_out() {
        // The kernel's `tramp_alias`, where the label is further down, in a section whose name
        // stands for its start. The words are what llvm-mc writes for the same text.
        let read = aarch64(concat!(
            "\t.text\n\t.macro tramp_alias, dst, sym\n",
            "\t.set .Lalias\\@, (0xffff800080000000 + 0x10000) + \\sym - .entry.tramp.text\n",
            "\tmovz \\dst, :abs_g2_s:.Lalias\\@\n\tmovk \\dst, :abs_g1_nc:.Lalias\\@\n",
            "\tmovk \\dst, :abs_g0_nc:.Lalias\\@\n\t.endm\n",
            "f:\ttramp_alias x29, tramp_exit\n\tret\n",
            "\t.pushsection \".entry.tramp.text\", \"ax\"\n\tnop\n\tnop\ntramp_exit:\n\tret\n\t.popsection\n",
        ));
        assert_eq!(words(&read, ".text"), [0x92cf_fffd, 0xf2b0_003d, 0xf280_011d, 0xd65f_03c0]);
    }

    #[test]
    fn a_branch_to_a_label_and_a_sum_of_labels_is_the_label_and_a_number() {
        // The KVM vectors of hyp-entry.S, each of which branches to the same place in another
        // table. The addends are the ones llvm-mc writes.
        let read = aarch64(concat!(
            "\t.text\n.macro hyp_ventry\n\t.align 7\n1:\tnop\n\tnop\n",
            "\tb __kvm_hyp_vector + (1b - 0b + (2 * 4))\n.endm\n",
            "\t.align 11\n0:\n\t.rept 4\n\thyp_ventry\n\t.endr\n",
        ));
        let relocs: Vec<_> =
            relocs(&read, ".text").into_iter().map(|(at, _, _, addend)| (at, addend)).collect();
        assert_eq!(relocs, [(8, 8), (0x88, 0x88), (0x108, 0x108), (0x188, 0x188)]);
    }

    #[test]
    fn reloc_asks_for_a_relocation_at_a_place_written_already() {
        // What the KVM build's gen-hyprel writes for each address the hypervisor code holds.
        let assembled = aarch64(concat!(
            ".data\n.pushsection .hyp.reloc, \"a\"\n.global __hyp_section_.text\n",
            ".word 0\n.reloc 0, R_AARCH64_PREL32, __hyp_section_.text + 0x18\n",
            ".word 0\n.reloc 4, R_AARCH64_PREL32, __hyp_section_.text + 0x2c\n",
            ".quad 0\n.reloc 8, R_AARCH64_ABS64, foo + 4\n.popsection\n",
        ));
        let relocs: Vec<_> = relocs(&assembled, ".hyp.reloc")
            .into_iter()
            .map(|(at, name, _, addend)| (at, name, addend))
            .collect();
        assert_eq!(
            relocs,
            [(0, "__hyp_section_.text", 0x18), (4, "__hyp_section_.text", 0x2c), (8, "foo", 4)]
        );
        for text in [
            ".data\n.word 0\n.reloc 0, R_AARCH64_CALL26, foo\n",
            ".data\n.word 0\n.reloc 4, R_AARCH64_PREL32, foo\n",
        ] {
            assert!(read(text, Arch::Aarch64).is_err(), "{text}");
        }
    }

    #[test]
    fn an_offset_that_is_the_distance_between_two_labels_is_a_number() {
        // The trampoline vectors of entry.S prefetch from where each one is in the table. The words
        // are the ones llvm-mc writes.
        let read = aarch64(concat!(
            "\t.text\n\t.align 7\nvs:\n\t.rept 2\n\t.align 7\n1:\tnop\n",
            "\tprfm plil1strm, [x30, #(1b - vs)]\n\tldr x0, [x1, #(1b - vs)]\n\t.endr\n",
            "\tprfm plil1strm, [x30, #(2f - vs)]\n\t.align 7\n2:\tnop\n",
        ));
        let words = words(&read, ".text");
        assert_eq!(words[..3], [0xd503_201f, 0xf980_03c9, 0xf940_0020]);
        assert_eq!(words[32..35], [0xd503_201f, 0xf980_43c9, 0xf940_4020]);
        assert_eq!(words[35], 0xf980_83c9);
    }

    #[test]
    fn a_label_in_front_of_a_conditional_is_defined() {
        // The GHASH loop of 6.12's ghash-ce-core.S, which goes back to a label written on the
        // `.ifc` line whichever way the test goes. The words are the ones llvm-mc writes.
        let assembled = aarch64(concat!(
            "\t.text\n.macro m pn\n0:\t.ifc \\pn, p64\n\tnop\n\t.endif\n\tcbnz w0, 0b\n.endm\n",
            "\tm p64\n\tm p8\n",
        ));
        assert_eq!(words(&assembled, ".text"), [0xd503_201f, 0x35ff_ffe0, 0x3500_0000]);
        // One in code that is not assembled is not there.
        let refused = read("\t.text\n.if 0\n3: .if 1\n.endif\n.endif\n\tb 3b\n", Arch::Aarch64);
        assert!(refused.unwrap_err().why.contains("'3b'"));
    }

    #[test]
    fn rep_is_rept() {
        let read = aarch64("\t.text\n\t.rep 3\n\t.word 0xe7fddef1\n\t.endr\n");
        assert_eq!(words(&read, ".text"), [0xe7fd_def1; 3]);
    }

    #[test]
    fn an_aarch64_line_that_is_not_an_instruction_is_refused_with_its_line() {
        let trouble = read("f:\n\tadd x0, x1, #zz\n", Arch::Aarch64).unwrap_err();
        assert_eq!(trouble.line, 2);
        let trouble = read("\tb 1f\n", Arch::Aarch64).unwrap_err();
        assert!(trouble.why.contains("'1f'"), "{trouble}");
        let trouble = read("\tcbz x0, far\n\t.skip 2000000\nfar:\tret\n", Arch::Aarch64);
        assert!(trouble.unwrap_err().why.contains("R_AARCH64_CONDBR19"));
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

    /// Under `-mrelax-relocations=no` every read of the table is the plain relocation, the way
    /// gas writes it, and on i386 the plain one of its own.
    #[test]
    fn a_kept_slot_is_never_one_the_linker_may_rewrite() {
        let flags = Flags { keep_slots: true, ..Flags::default() };
        let text = "\t.text\n\tmovq f@GOTPCREL(%rip), %rax\n\tcall *g@GOTPCREL(%rip)\n";
        let out = read_with(text, Arch::X86_64, ObjectFormat::Elf, flags).expect("it reads");
        let kinds: Vec<_> = out.parts[0].relocs.iter().map(|reloc| reloc.kind).collect();
        assert_eq!(kinds, [Reference::GotKept, Reference::GotKept]);
        let text = "\t.text\n\tmovl f@GOT(%ebx), %eax\n";
        let out = read_with(text, Arch::X86, ObjectFormat::Elf, flags).expect("it reads");
        assert_eq!(out.parts[0].relocs[0].kind, Reference::SlotKept);
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
        assert_eq!(foo.at, Held::In { part: 1, offset: 4 });
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
    fn octa_is_sixteen_bytes_of_a_number_as_wide_as_that() {
        let out = assembled(
            "\t.data\n\t.octa 0x000102030405060708090a0b0c0d0e0f\n\t.octa -1, 2\n\t.octa 1+2\n",
        );
        let mut want: Vec<u8> = (0..16).rev().collect();
        want.extend_from_slice(&[0xff; 16]);
        want.extend_from_slice(&2u128.to_le_bytes());
        want.extend_from_slice(&3u128.to_le_bytes());
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
    fn incbin_puts_the_bytes_of_a_file_there() {
        let path = std::env::temp_dir().join(format!("rucc-incbin-{}.bin", std::process::id()));
        std::fs::write(&path, b"abcdef").unwrap();
        // A string in the source reads a backslash as an escape, and a Windows path is full of them.
        let name = path.display().to_string().replace('\\', "\\\\");
        let out =
            assembled(&format!("\t.data\n\t.incbin \"{name}\"\n\t.incbin \"{name}\", 2, 3\n"));
        let missing = read(&format!("\t.data\n\t.incbin \"{name}\", 7\n"), Arch::X86_64);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(bytes(&out, ".data"), b"abcdefcde".to_vec());
        assert!(missing.is_err(), "a skip past the end of the file is refused");
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
        assert_eq!(out.parts[1].align, 16, "the section has to start where the widest ask does");
    }

    #[test]
    fn a_fill_of_the_nop_byte_in_code_is_long_nops_the_way_gas_writes_them() {
        let asked = assembled("\t.text\n\tret\n\t.align 16, 0x90\n\tret\n");
        let plain = assembled("\t.text\n\tret\n\t.align 16\n\tret\n");
        assert_eq!(bytes(&asked, ".text"), bytes(&plain, ".text"));
        assert_ne!(
            bytes(&asked, ".text")[1],
            0x90,
            "fifteen bytes of padding are not one byte long"
        );
        // Any other fill, or the same one in data, is the byte it says.
        let other = assembled("\t.text\n\tret\n\t.align 4, 0xcc\n\tret\n");
        assert_eq!(bytes(&other, ".text"), vec![0xc3, 0xcc, 0xcc, 0xcc, 0xc3]);
        let data = assembled("\t.data\n\t.byte 1\n\t.align 4, 0x90\n");
        assert_eq!(bytes(&data, ".data"), vec![1, 0x90, 0x90, 0x90]);
    }

    /// i386 code is padded with the `lea` forms gas writes there, not x86-64's `nopl`. gas picks the
    /// forms by the machine the object is for, so x86-64 code after `.code32` still gets `nopl`,
    /// and i386 code after `.code64` gets the `lea` forms with a REX.W.
    #[test]
    fn padding_in_thirty_two_bit_code_is_what_gas_writes_for_i386() {
        let out = read("\t.text\n\tret\n\t.p2align 4\n\tret\n", Arch::X86).unwrap();
        let text = bytes(&out, ".text");
        assert_eq!(text.len(), 17);
        assert_eq!(text[1..16], [0x2e, 0x8d, 0xb4, 0x26, 0, 0, 0, 0, 0x8d, 0xb4, 0x26, 0, 0, 0, 0]);
        let out = read("\t.text\n\tret\n\t.p2align 2\n\tret\n", Arch::X86).unwrap();
        assert_eq!(bytes(&out, ".text"), [0xc3, 0x8d, 0x76, 0x00, 0xc3]);
        let code32 = assembled("\t.text\n\t.code32\n\tret\n\t.p2align 2\n\tret\n");
        assert_eq!(bytes(&code32, ".text"), [0xc3, 0x0f, 0x1f, 0x00, 0xc3]);
        let code64 = read("\t.text\n\t.code64\n\tret\n\t.p2align 2\n\tret\n", Arch::X86).unwrap();
        assert_eq!(bytes(&code64, ".text"), [0xc3, 0x48, 0x89, 0xf6, 0xc3]);
        let long = assembled("\t.text\n\tret\n\t.p2align 2\n\tret\n");
        assert_eq!(bytes(&long, ".text"), [0xc3, 0x0f, 0x1f, 0x00, 0xc3]);
    }

    /// Padding after data starts with a one byte `nop`, as gas 2.42 writes it, and the section
    /// remembers the data across an alignment and a switch to another section and back. An
    /// instruction in between is padding as usual.
    #[test]
    fn padding_after_data_starts_with_a_one_byte_nop() {
        let read = assembled(concat!(
            "\t.section .a,\"ax\"\n\tret\n\t.byte 0xc3\n\t.p2align 3\n",
            "\t.section .b,\"ax\"\n\tret\n\t.skip 1, 0x90\n\t.section .z,\"ax\"\n\tnop\n",
            "\t.section .b,\"ax\"\n\t.p2align 2\n\t.p2align 3\n",
            "\t.section .c,\"ax\"\n\t.byte 0xc3\n\tret\n\t.p2align 3\n",
        ));
        assert_eq!(bytes(&read, ".a"), [0xc3, 0xc3, 0x90, 0x0f, 0x1f, 0x44, 0x00, 0x00]);
        assert_eq!(bytes(&read, ".b"), [0xc3, 0x90, 0x90, 0x90, 0x90, 0x0f, 0x1f, 0x00]);
        assert_eq!(bytes(&read, ".c"), [0xc3, 0xc3, 0x66, 0x0f, 0x1f, 0x44, 0x00, 0x00]);
    }

    /// gas 2.40 pads with the table for the mode the file ends in and the machine the object is
    /// for: in an i386 object the old `lea` forms with no `cs` prefix, after `.code16` at the end the
    /// sixteen bit ones everywhere, and a jump over a long run. The bytes are the ones gas 2.40
    /// writes for the same lines.
    #[test]
    fn padding_before_gas_2_42_is_picked_by_how_the_file_ends() {
        let old = Flags { before_2_42: true, ..Flags::default() };
        let text = "\t.section .a,\"ax\"\n\tret\n\t.p2align 4\n\t.section .b,\"ax\"\n\tret\n\t.p2align 5\n";
        let read = read_with(text, Arch::X86, ObjectFormat::Elf, old).expect("it reads");
        let mut want = vec![0xc3, 0x8d, 0xb4, 0x26, 0, 0, 0, 0, 0x8d, 0xb4, 0x26, 0, 0, 0, 0, 0x90];
        assert_eq!(bytes(&read, ".a"), want);
        want = vec![0xc3, 0xeb, 0x1d];
        for _ in 0..4 {
            want.extend_from_slice(&[0x8d, 0xb4, 0x26, 0, 0, 0, 0]);
        }
        want.push(0x90);
        assert_eq!(bytes(&read, ".b"), want);

        let text = "\t.code32\n\tret\n\t.p2align 8\n\t.code16\n\tret\n\t.p2align 3\n";
        let read = read_with(text, Arch::X86, ObjectFormat::Elf, old).expect("it reads");
        let code = bytes(&read, ".text");
        assert_eq!(code[..11], [0xc3, 0x66, 0xe9, 0xf9, 0, 0, 0, 0x8d, 0xb4, 0x00, 0x00]);
        assert_eq!(code[256..], [0xc3, 0x8d, 0xb4, 0x00, 0x00, 0x8d, 0x74, 0x00]);

        let text = "\tret\n\t.p2align 8\n";
        let read = read_with(text, Arch::X86_64, ObjectFormat::Elf, old).expect("it reads");
        let code = bytes(&read, ".text");
        assert_eq!(code[..8], [0xc3, 0xe9, 0xfa, 0, 0, 0, 0x66, 0x66]);
    }

    /// gas 2.40 writes `lar` and `lsl` into a sixty four bit register with `REX.W`, and gas 2.42
    /// leaves it out. The bytes are the ones each writes for the same lines.
    #[test]
    fn lsl_into_a_quad_register_has_rex_w_before_gas_2_42() {
        let text = "\tlsl %rax, %rax\n\tlsl %r9, %r10\n\tlslq (%rax), %rcx\n\tlar %cx, %rbx\n\tlsl %eax, %eax\n";
        let old = Flags { before_2_42: true, ..Flags::default() };
        let read = read_with(text, Arch::X86_64, ObjectFormat::Elf, old).expect("it reads");
        let want = [
            0x48, 0x0f, 0x03, 0xc0, 0x4d, 0x0f, 0x03, 0xd1, 0x48, 0x0f, 0x03, 0x08, 0x48, 0x0f,
            0x02, 0xd9, 0x0f, 0x03, 0xc0,
        ];
        assert_eq!(bytes(&read, ".text"), want);
        let read = assembled(text);
        let want = [
            0x0f, 0x03, 0xc0, 0x45, 0x0f, 0x03, 0xd1, 0x0f, 0x03, 0x08, 0x0f, 0x02, 0xd9, 0x0f,
            0x03, 0xc0,
        ];
        assert_eq!(bytes(&read, ".text"), want);
    }

    /// gas 2.40 pads after data as it pads after an instruction, which is what a kernel built for
    /// the E10 era is assembled with.
    #[test]
    fn padding_after_data_is_plain_before_gas_2_42() {
        let text = "\t.section .a,\"ax\"\n\tret\n\t.byte 0xc3\n\t.p2align 3\n\t.p2align 4\n";
        let old = Flags { before_2_42: true, ..Flags::default() };
        let read = read_with(text, Arch::X86_64, ObjectFormat::Elf, old).expect("it reads");
        let mut want = vec![0xc3, 0xc3, 0x66, 0x0f, 0x1f, 0x44, 0x00, 0x00];
        want.extend_from_slice(&[0x0f, 0x1f, 0x84, 0x00, 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(bytes(&read, ".a"), want);
    }

    #[test]
    fn a_section_that_holds_no_bytes_counts_them_rather_than_carrying_them() {
        let out = assembled("\t.bss\n\t.globl room\nroom:\n\t.zero 4096\n");
        let part = &out.parts[2];
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
    fn a_section_gas_knows_by_name_keeps_the_flags_the_name_gives_it() {
        let out = assembled(
            "\t.section .data.rel.ro.local,\"a\",@progbits\n\t.quad 0\n\
             \t.section .text.hot,\"a\",@progbits\n\t.byte 0x90\n\
             \t.section .init.data,\"aw\",@progbits\n\t.byte 1\n",
        );
        let shape = |name: &str| out.parts.iter().find(|part| part.name == name).unwrap().shape;
        assert!(shape(".data.rel.ro.local").write, "a jump table the linker cannot fix up");
        assert!(shape(".text.hot").exec);
        assert!(!shape(".init.data").exec, "only `.init` itself is code");
    }

    /// What rucc writes in front of a function in a section of its own, under
    /// `-fpatchable-function-entry`. Each record goes with its own function's text, so two of them
    /// are two sections even though they share a name, and naming the first function again goes
    /// back to the first one.
    #[test]
    fn a_section_that_goes_with_a_name_is_one_section_for_each_name() {
        let out = assembled(
            "\t.section .init.text,\"ax\",@progbits\nf:\tret\n\
             \t.section __patchable_function_entries,\"awo\",@progbits,f\n\t.quad f\n\
             \t.text\ng:\tret\n\
             \t.section __patchable_function_entries,\"awo\",@progbits,g\n\t.quad g\n\
             \t.section __patchable_function_entries,\"awo\",@progbits,f\n\t.quad f\n",
        );
        let records: Vec<_> =
            out.parts.iter().filter(|part| part.name == "__patchable_function_entries").collect();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].link.as_deref(), Some("f"));
        assert_eq!(records[0].bytes.len(), 16);
        assert_eq!(records[1].link.as_deref(), Some("g"));
        assert_eq!(records[1].bytes.len(), 8);
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

    /// `.previous` after a push and a pop goes where it would have gone without them. Linux 5.15's
    /// `.Lbad_gs` in entry_64.S is an `ALTERNATIVE` inside `.section .fixup` and then `.previous`,
    /// and going back to `.altinstr_replacement` there put `error_entry` and everything after it
    /// in init memory, which the kernel frees once it has booted.
    #[test]
    fn previous_after_a_push_and_a_pop_goes_back_past_them() {
        let out = assembled(
            "\t.text\n\t.byte 1\n\t.section .fixup, \"ax\"\n\t.byte 2\n\
             \t.pushsection .altinstr_replacement, \"ax\"\n\t.byte 3\n\t.popsection\n\
             \t.byte 4\n\t.previous\n\t.byte 5\n",
        );
        assert_eq!(bytes(&out, ".text"), vec![1, 5]);
        assert_eq!(bytes(&out, ".fixup"), vec![2, 4]);
        assert_eq!(bytes(&out, ".altinstr_replacement"), vec![3]);
    }

    /// A subsection goes behind the section it is part of, in the order of the numbers and not the
    /// order the file wrote them in, and each spelling of one reaches the same run: `.subsection`,
    /// a number after `.text`, and a number after the name in `.pushsection`. A jump from one run
    /// to another is a distance inside one section once they are put together, and `.previous`
    /// goes back to the run the file was in and not to the section as a whole.
    #[test]
    fn subsections_go_behind_their_section_in_the_order_of_their_numbers() {
        let out = assembled(
            "\t.text\n\t.byte 1\n\t.subsection 2\n\t.byte 5\n\t.text 1\n2:\n\t.byte 3\n\
             \t.subsection 0\n\t.byte 2\n\t.pushsection .text, 1\n\t.byte 4\n\t.popsection\n\
             \tjmp 2b\n\t.section .data\n\t.byte 9\n\t.previous\n\t.byte 0xcc\n",
        );
        // The jump back to `2:` is written after the unnumbered run, which ends with it and the
        // `int3` behind it, and the label is the first byte of subsection 1 behind those.
        assert_eq!(bytes(&out, ".text"), vec![1, 2, 0xeb, 1, 0xcc, 3, 4, 5]);
        assert_eq!(bytes(&out, ".data"), vec![9]);
        assert_eq!(out.parts.iter().filter(|part| part.name == ".text").count(), 1);

        // A run that asked for a boundary starts on it, so the alignment inside it still holds.
        let out = assembled("\t.data\n\t.byte 1\n\t.subsection 1\n\t.balign 4\n\t.byte 2\n");
        assert_eq!(bytes(&out, ".data"), vec![1, 0, 0, 0, 2]);
    }

    /// The same numbered label written by two templates is two places, and a table entry each
    /// template pushes into a section of its own names the one behind it. That is what every
    /// `_ASM_EXTABLE` in the kernel is, and what the compiler's listing hands this with every
    /// template of a unit in one stream.
    #[test]
    fn numbered_labels_and_pushed_sections_are_one_stream() {
        let entry = "\t.pushsection __ex_table,\"a\"\n\t.balign 4\n\t.long 1b - .\n\t.long 2f - .\n\
                     \t.popsection\n";
        let out = assembled(&format!("1:\tnop\n{entry}2:\tnop\n1:\tnop\n{entry}2:\tret\n"));
        assert_eq!(bytes(&out, ".text"), vec![0x90, 0x90, 0x90, 0xc3]);
        let table = out.parts.iter().find(|part| part.name == "__ex_table").unwrap();
        // Where in `.text` each entry points, from the place each relocation names and its addend.
        let relocs: Vec<(usize, i64)> = table
            .relocs
            .iter()
            .map(|reloc| match name(&out, &reloc.symbol).at {
                Held::In { offset, .. } => (reloc.at, offset as i64 + reloc.addend),
                other => panic!("'{}' is {other:?}", reloc.symbol),
            })
            .collect();
        assert_eq!(relocs, [(0, 0), (4, 1), (8, 2), (12, 3)]);
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
    fn an_ident_is_a_string_in_the_comment_section() {
        let out = assembled("\t.text\n\t.ident \"one\"\n\t.ident \"two\"\n\tret\n");
        assert_eq!(bytes(&out, ".comment"), b"\0one\0two\0");
        assert_eq!(bytes(&out, ".text"), [0xc3]);
        let comment = out.parts.iter().find(|part| part.name == ".comment").expect("the section");
        assert_eq!(
            (comment.shape.alloc, comment.shape.merge, comment.shape.strings),
            (false, 1, true)
        );
    }

    #[test]
    fn the_stack_marker_is_added_only_when_asked_for() {
        // gas writes no `.note.GNU-stack` of its own, and the kernel's `.S` files are assembled
        // without `--noexecstack`, so its section checks find none in gcc's objects.
        let marked = |text: &str, noexecstack: bool| {
            let flags = Flags { noexecstack, ..Flags::default() };
            let out = read_with(text, Arch::X86_64, ObjectFormat::Elf, flags).expect("a file");
            out.parts.iter().filter(|part| part.name == ".note.GNU-stack").count()
        };
        assert_eq!(marked("\tret\n", false), 0);
        assert_eq!(marked("\tret\n", true), 1);
        let said = "\tret\n\t.section .note.GNU-stack,\"\",@progbits\n";
        assert_eq!((marked(said, false), marked(said, true)), (1, 1));
    }

    #[test]
    fn a_name_set_to_a_function_is_a_function_of_the_same_size() {
        // How gcc writes `__attribute__((alias))`, and what objtool goes by: the kernel's syscall
        // stubs are aliases, and one that came out a bare label got no `__pfx_` in front of it.
        let out = assembled(
            "\t.text\n\t.type f, @function\nf:\n\t.byte 0,0,0\n\t.size f, .-f\n\t.globl g\n\t.set \
             g,f\n\t.set h, f+1\n",
        );
        assert_eq!((name(&out, "g").sort, name(&out, "g").size), (Sort::Func, 3));
        assert_eq!((name(&out, "h").sort, name(&out, "h").size), (Sort::Untyped, 0));
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
        let reloc = &out.parts[1].relocs[0];
        assert_eq!(reloc.at, 0);
        assert_eq!(reloc.symbol, "message");
        assert_eq!(reloc.kind, Reference::Address { bytes: 8 });
        assert_eq!(reloc.addend, 0);
        assert_eq!(name(&out, "message").at, Held::Undefined);
    }

    #[test]
    fn code_that_is_not_position_independent_reaches_its_data_by_address() {
        // What gcc writes under `-fno-pie`. The immediate of `movl` is the address as it is, and
        // the displacement and the immediate of `movq` are sign extended, which the linker is told
        // so that it checks the address fits that way. A name in this section is still an address
        // the linker writes, since where the section lands is not known here either.
        let out = assembled(
            "\t.text\nf:\n\tmovl $.LC0, %edi\n\tmovq $.LC0+4, %rdi\n\tmovl \
             table(,%rax,4), %eax\n\tmovabsq $f, %rax\n\tret\n\t.section .rodata\n.LC0:\n\t\
             .string \"hi\"\n",
        );
        let text = out.parts.iter().find(|part| part.name == ".text").unwrap();
        let kinds: Vec<_> =
            text.relocs.iter().map(|reloc| (reloc.at, reloc.kind, reloc.addend)).collect();
        assert_eq!(
            kinds,
            [
                (1, Reference::Address { bytes: 4 }, 0),
                (8, Reference::Signed, 4),
                (15, Reference::Signed, 0),
                (21, Reference::Address { bytes: 8 }, 0),
            ]
        );
        assert_eq!(text.relocs[2].symbol, "table");
    }

    #[test]
    fn a_call_through_a_name_with_no_registers_is_through_that_address() {
        // The kernel's paravirt calls before 6.5 are `call *pv_ops+16`, a call through the slot
        // at that address, and gas writes the address sign extended as it does for a `movq`.
        let out = assembled("\t.text\nf:\n\tcall *pv_ops+16\n\tjmp *pv_ops+8\n");
        let text = out.parts.iter().find(|part| part.name == ".text").unwrap();
        let kinds: Vec<_> = text
            .relocs
            .iter()
            .map(|reloc| (reloc.at, reloc.symbol.as_str(), reloc.kind, reloc.addend))
            .collect();
        assert_eq!(
            kinds,
            [(3, "pv_ops", Reference::Signed, 16), (10, "pv_ops", Reference::Signed, 8)]
        );
    }

    #[test]
    fn a_name_in_one_or_two_bytes_is_an_address_that_narrow_on_elf_and_refused_on_windows() {
        // gas writes `R_X86_64_16` and `R_X86_64_8` for these and leaves the linker to check the
        // address fits. A Windows object has no relocation that narrow.
        let out = assembled("\t.text\n\tmovw $message, %ax\n\t.data\n\t.byte sym\n\t.short sym\n");
        let text = out.parts.iter().find(|part| part.name == ".text").unwrap();
        assert_eq!(text.bytes, [0x66, 0xb8, 0, 0]);
        assert_eq!(text.relocs[0].kind, Reference::Address { bytes: 2 });
        let data = out.parts.iter().find(|part| part.name == ".data").unwrap();
        let kinds: Vec<_> = data.relocs.iter().map(|reloc| (reloc.at, reloc.kind)).collect();
        assert_eq!(
            kinds,
            [(0, Reference::Address { bytes: 1 }), (1, Reference::Address { bytes: 2 })]
        );
        let Err(why) = read_as("\t.text\n\tmovw $message, %ax\n", Arch::X86_64, ObjectFormat::Coff)
        else {
            panic!("read");
        };
        assert!(why.why.contains("bytes"), "{why}");
    }

    #[test]
    fn a_distance_from_here_to_something_else_is_a_relocation_relative_to_here() {
        // The other shape a reduced expression can have, and the one whose addend is not zero: the
        // four bytes sit at offset four, and a relocation counts from where it starts.
        let out = assembled("\t.data\n\t.quad 0\n\t.long message - .\n");
        let reloc = &out.parts[1].relocs[0];
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
        let reloc = &out.parts[1].relocs[0];
        assert_eq!(reloc.at, 8);
        assert_eq!(reloc.kind, Reference::Data);
        assert_eq!(reloc.addend, 8);
    }

    #[test]
    fn a_section_named_where_a_name_was_expected_is_its_start() {
        // The vDSO's exception table, which counts each place from the start of the table. The
        // second entry is four bytes in, so its distance from the start is four more than from
        // these bytes. The name itself is not in the table and a lone reference is to the section.
        let out = assembled(
            "\t.text\nf:\tnop\n.Lfrom:\tnop\n.Lto:\tret\n\
             \t.pushsection __ex_table, \"a\"\n\
             \t.long (.Lfrom) - __ex_table\n\t.long (.Lto) - __ex_table\n\t.popsection\n\
             \t.section .rodata\n\t.quad __ex_table + 4\n",
        );
        let table = out.parts.iter().find(|part| part.name == "__ex_table").expect("the table");
        let got: Vec<_> =
            table.relocs.iter().map(|r| (r.at, r.symbol.as_str(), r.addend)).collect();
        assert_eq!(got, [(0, ".Lfrom", 0), (4, ".Lto", 4)]);
        assert!(table.relocs.iter().all(|reloc| reloc.kind == Reference::Data));
        assert!(out.names.iter().all(|name| name.name != "__ex_table"));
        let rodata = out.parts.iter().find(|part| part.name == ".rodata").expect("rodata");
        assert_eq!(rodata.relocs[0].symbol, "__ex_table\u{1}start");
        assert_eq!(rodata.relocs[0].addend, 4);
    }

    #[test]
    fn a_number_added_to_a_name_rides_along_in_the_addend() {
        let out = assembled("\t.data\n\t.quad message + 16\n");
        assert_eq!(out.parts[1].relocs[0].addend, 16);
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

    /// gas gives local commons the room after everything the file put in `.bss` itself, wherever
    /// the line was, which is how gcc's `static char stack[4096]` lands after the variables it
    /// writes out in `.bss` in arch/x86/kernel/acpi/sleep.c.
    #[test]
    fn a_local_common_comes_after_what_the_file_put_in_bss() {
        let out = assembled(
            "\t.text\n\t.local stack\n\t.comm stack,4096,32\n\tret\n\t.bss\nflags:\n\t.zero 9\n",
        );
        let Held::In { part, offset } = name(&out, "stack").at else { panic!("not in a section") };
        assert_eq!(out.parts[part].name, ".bss");
        assert_eq!(offset, 32);
        assert_eq!(name(&out, "flags").at, Held::In { part, offset: 0 });
        assert_eq!(out.parts[part].size, 32 + 4096);
    }

    /// mpparse.c's `.bss`, where gas puts a common byte straight after the four bytes the file
    /// wrote and aligns the common word after it from there.
    #[test]
    fn a_local_common_is_aligned_where_it_lands() {
        let out = assembled(
            "\t.bss\n\t.align 4\nmine:\n\t.zero 4\n\t.data\n\t.local found\n\t.comm \
             found,1,1\n\t.local base\n\t.comm base,8,8\n",
        );
        let Held::In { part, offset } = name(&out, "found").at else { panic!("not in a section") };
        assert_eq!(offset, 4);
        assert_eq!(name(&out, "base").at, Held::In { part, offset: 8 });
        assert_eq!((out.parts[part].size, out.parts[part].align), (16, 8));
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
        let why =
            refused("\t.text\nf:\n\tmovq %rdi, %rax\n\tvpdpbusd %zmm1, %zmm2, %zmm3\n\tret\n");
        assert_eq!(why.line, 4);
        assert!(why.why.contains("vpdpbusd"), "{why}");
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
            out.parts.iter().skip(3).map(|part| (part.shape.merge, part.shape.strings)).collect();
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
    fn a_loop_counts_down_with_one_byte_of_reach() {
        // `relocate_kernel_64.S` copies a page with `loop`, which like `jrcxz` has only the short
        // form.
        let out = assembled(
            "\t.text\nagain:\n\tnop\n\tloop again\n\tloopne again\n\tloope again\n\tret\n",
        );
        assert_eq!(bytes(&out, ".text"), vec![0x90, 0xe2, 0xfd, 0xe0, 0xfb, 0xe1, 0xf9, 0xc3]);
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
    fn the_three_sections_gas_starts_with_are_there_however_empty() {
        // gas makes `.text`, `.data` and `.bss` before it reads a line, and the kernel's section
        // checks compare an object of this against one of gas's. A section only ever mentioned by
        // its short name is still dropped on the other formats.
        let out = assembled("\t.data\n\t.byte 1\n");
        let names: Vec<&str> = out.parts.iter().map(|part| part.name.as_str()).collect();
        assert_eq!(names, [".text", ".data", ".bss"]);
        let coff =
            read_with("\t.data\n\t.byte 1\n", Arch::X86_64, ObjectFormat::Coff, Flags::default())
                .expect("a file of one byte");
        assert_eq!(coff.parts.len(), 1);
        assert_eq!(coff.parts[0].name, ".data");
    }

    #[test]
    fn a_section_with_nothing_in_it_but_a_name_is_kept() {
        // Because the name has to point somewhere, and dropping the section under it would leave a
        // symbol pointing at a section that is not there.
        let out = assembled("\t.text\n\t.globl marker\nmarker:\n");
        assert_eq!(out.parts[0].name, ".text");
        assert_eq!(name(&out, "marker").at, Held::In { part: 0, offset: 0 });
    }

    #[test]
    fn an_error_directive_is_the_file_saying_it_refuses_itself() {
        let why = refused("\t.error \"this is not the machine for it\"\n");
        assert!(why.why.contains("not the machine for it"), "{why}");
    }

    #[test]
    fn a_warning_directive_stops_the_file_only_when_warnings_are_fatal() {
        // gas prints the warning and writes the object, and under `--fatal-warnings` it writes
        // nothing. The kernel's `as-instr` probes pass that flag, so a probe that meets a warning
        // has to fail here too or the feature it probes for is switched on wrongly.
        let text = "\t.warning \"old enough to complain about\"\n\tret\n";
        read(text, Arch::X86_64).expect("a warning is not an error by itself");
        let fatal = Flags { fatal_warnings: true, ..Flags::default() };
        let why = read_with(text, Arch::X86_64, ObjectFormat::Elf, fatal)
            .expect_err("--fatal-warnings let a warning through");
        assert!(why.why.contains("old enough to complain about"), "{why}");
        assert!(why.why.contains("--fatal-warnings"), "{why}");
        read_with("\tret\n", Arch::X86_64, ObjectFormat::Elf, fatal)
            .expect("a file with no warning in it is still read");
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
    fn a_number_an_instruction_carries_may_not_be_two_names_elsewhere() {
        let why = refused("\tmov $elsewhere-there, %eax\n");
        assert!(why.why.contains("relocation"), "{why}");
    }

    /// The relocations of the section of that name, as where, against what, which kind and the
    /// addend, which is everything a test of one of them looks at.
    fn relocated(assembled: &Assembled, name: &str) -> Vec<(usize, String, Reference, i64)> {
        let part = assembled.parts.iter().find(|part| part.name == name).unwrap();
        part.relocs
            .iter()
            .map(|reloc| (reloc.at, reloc.symbol.clone(), reloc.kind, reloc.addend))
            .collect()
    }

    /// The shape of the kernel's `arch/x86/boot/compressed/head_64.S`, which reaches its data and
    /// the end of its image from a register holding where `startup_32` was loaded. gas writes each
    /// name less a label of this section as the distance from the four bytes to the name, with the
    /// distance from the label to the bytes as the addend, and these are the bytes and relocations
    /// `llvm-mc` writes for the same lines. The label is taken away both before and after it is
    /// defined, and in thirty two bit code as well as sixty four.
    #[test]
    fn a_name_less_a_label_of_this_section_is_the_distance_to_the_name_from_here() {
        let out = assembled(
            "\t.section .head.text,\"ax\"\n\t.code32\nstartup_32:\n\tnop\n\
             \tleal ((gdt) - startup_32)(%ebp), %eax\n\
             \tleal ((gdt) - later)(%ebp), %eax\n\
             \tsubl $ ((_end) - startup_32), %ebx\n\
             \tmovl $(boot_stack_end - later), %ecx\n\
             later:\n\
             \tleal ((pgtable + 0x1000) - startup_32)(%ebx), %edi\n\
             \tmovl $(_bss - later), %ecx\n\
             \t.code64\n\
             \tleaq ((top_pgtable) - startup_32)(%rbx), %rsi\n\
             \tleaq ((gdt) - startup_32)(%rbx), %rdx\n\
             \tsubq $((_end) - last), %rbx\n\
             last:\n\
             \t.data\ngdt:\t.quad 0\n\t.bss\nboot_stack_end:\t.skip 8\n",
        );
        #[rustfmt::skip]
        let want: [u8; 0x38] = [
            0x90,
            0x8d, 0x85, 0, 0, 0, 0,
            0x8d, 0x85, 0, 0, 0, 0,
            0x81, 0xeb, 0, 0, 0, 0,
            0xb9, 0, 0, 0, 0,
            0x8d, 0xbb, 0, 0, 0, 0,
            0xb9, 0, 0, 0, 0,
            0x48, 0x8d, 0xb3, 0, 0, 0, 0,
            0x48, 0x8d, 0x93, 0, 0, 0, 0,
            0x48, 0x81, 0xeb, 0, 0, 0, 0,
        ];
        assert_eq!(bytes(&out, ".head.text"), want);
        let pc = Reference::Data;
        assert_eq!(
            relocated(&out, ".head.text"),
            [
                (0x03, "gdt".to_owned(), pc, 3),
                (0x09, "gdt".to_owned(), pc, 0x09 - 0x18),
                (0x0f, "_end".to_owned(), pc, 0x0f),
                (0x14, "boot_stack_end".to_owned(), pc, 0x14 - 0x18),
                (0x1a, "pgtable".to_owned(), pc, 0x1000 + 0x1a),
                (0x1f, "_bss".to_owned(), pc, 0x1f - 0x18),
                (0x26, "top_pgtable".to_owned(), pc, 0x26),
                (0x2d, "gdt".to_owned(), pc, 0x2d),
                (0x34, "_end".to_owned(), pc, 0x34 - 0x38),
            ]
        );
    }

    /// The same two lines in a file for i386, where the relocation is `R_386_PC32` and the addend
    /// is written into the bytes rather than kept beside them, which is the object writer's
    /// business and not this one's.
    #[test]
    fn a_name_less_a_label_of_this_section_is_the_same_distance_on_i386() {
        let out = read(
            "\t.text\nstartup_32:\n\tnop\n\tleal ((gdt) - startup_32)(%ebp), %eax\n\
             \tsubl $ ((_end) - later), %ebx\nlater:\n\t.data\ngdt:\t.long 0\n",
            Arch::X86,
        )
        .unwrap();
        assert_eq!(bytes(&out, ".text"), [0x90, 0x8d, 0x85, 0, 0, 0, 0, 0x81, 0xeb, 0, 0, 0, 0]);
        let pc = Reference::Data;
        assert_eq!(
            relocated(&out, ".text"),
            [(3, "gdt".to_owned(), pc, 3), (9, "_end".to_owned(), pc, -4)]
        );
    }

    /// Two labels of this section in a displacement come out as the number between them, in the
    /// four bytes the address was given while that number was not known yet.
    #[test]
    fn a_label_less_a_label_of_this_section_in_a_displacement_is_a_number() {
        let out =
            assembled(".code32\nstartup_32:\n\tnop\n1:\tleal ((1b) - startup_32)(%ebp), %eax\n");
        assert_eq!(bytes(&out, ".text"), [0x90, 0x8d, 0x85, 1, 0, 0, 0]);
        assert!(relocated(&out, ".text").is_empty());
    }

    /// A name less a label is only a relocation when the label is in the section the bytes are,
    /// and a name less a name that nothing here defines is not one at all.
    #[test]
    fn a_name_less_a_label_somewhere_else_is_refused() {
        let other = "\t.text\n\tleal ((gdt) - there)(%ebp), %eax\n\t.data\nthere:\n\t.long 0\n";
        let why = refused(other);
        assert!(why.why.contains("another section"), "{why}");
        let other = "\t.text\n\tmovl $(gdt - there), %eax\n\t.data\nthere:\n\t.long 0\n";
        let why = refused(other);
        assert!(why.why.contains("another section"), "{why}");
        let why = refused("\t.text\n\tleal ((gdt) - there)(%ebp), %eax\n");
        assert!(why.why.contains("relocation"), "{why}");
        let why = refused("here:\n\tleaq (gdt - here)(%rip), %rax\n");
        assert!(why.why.contains("counted from the instruction"), "{why}");
        let why = refused("here:\n\tleaq (gdt - here - there)(%rbx), %rax\n");
        assert!(why.why.contains("two names taken away"), "{why}");
    }

    #[test]
    fn a_name_set_twice_means_what_it_was_where_it_is_used() {
        let out = assembled(
            "\t.data\n\t.byte early\n\tearly = 3\n\tx = 1\n\t.byte x\n\tx = x + 1\n\t.byte x\n",
        );
        assert_eq!(bytes(&out, ".data"), vec![3, 1, 2]);
    }

    #[test]
    fn a_name_set_twice_is_in_the_table_with_the_last_value() {
        let out = assembled(
            "\t.text\n\t.set i, 0\n\t.rept 3\n\taddl $i, %eax\n\t.set i, i+1\n\t.endr\n\t.globl \
             level\n\t.set level, 2\n\t.set level, 5\n",
        );
        assert_eq!(name(&out, "i").at, Held::Absolute(3));
        assert_eq!(name(&out, "level").at, Held::Absolute(5));
        assert_eq!(name(&out, "level").binding, Binding::Global);
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

    /// gas's table of names: a name it has no entry for gets no flags, a dotted child of one it
    /// has gets that one's, and a note stays a note when letters come with no type.
    #[test]
    fn a_section_with_no_letters_gets_what_gas_gives_its_name() {
        let out = assembled(
            "\t.pushsection .discard.x\n\t.long 1\n\t.popsection\n\t.section .text.y\n\tret\n\t.section \
             .note.gnu.property,\"a\"\n\t.long 4\n\t.section .bss.z,\"aw\"\n\t.zero 4\n\t.section \
             .init.text\n\t.section .data..percpu\n",
        );
        let shape = |name: &str| out.parts.iter().find(|part| part.name == name).expect(name).shape;
        assert_eq!(shape(".discard.x"), Shape { bits: true, ..Shape::default() });
        assert!(shape(".text.y").exec && shape(".text.y").alloc);
        let note = shape(".note.gnu.property");
        assert!(note.note && note.alloc && !note.write);
        assert!(!shape(".bss.z").bits);
        assert_eq!(shape(".init.text"), Shape { bits: true, ..Shape::default() });
        assert!(shape(".data..percpu").write);
    }

    /// `"a" "b"` is one string, as gas reads it, and `.asciz` ends it once.
    #[test]
    fn strings_side_by_side_are_one_string() {
        let out = assembled(
            "\t.data\n\t.asciz \"a\" \"b\", \"c\"\n\t.ascii \"\" \"\\0\"\n\t.ascii \"q\\\"\" \"r\"\n",
        );
        let data = out.parts.iter().find(|part| part.name == ".data").expect("data");
        assert_eq!(data.bytes, b"ab\0c\0\0q\"r");
    }

    /// What gas writes for the same text: a header marked by all ones, version one and no
    /// augmentation, then a record naming the header's offset and the function's address and
    /// length, padded to a pointer in a section that is not loaded.
    #[test]
    fn frame_rules_for_a_debugger_go_in_the_debuggers_copy() {
        let out =
            assembled("\t.cfi_sections .debug_frame\n\t.cfi_startproc\n\tret\n\t.cfi_endproc\n");
        let table = out.parts.iter().find(|part| part.name == ".debug_frame").expect("a table");
        assert!(!table.shape.alloc);
        assert_eq!(table.align, 8);
        assert_eq!(table.bytes.len(), 0x30);
        assert_eq!(table.bytes[4..10], [0xff, 0xff, 0xff, 0xff, 1, 0]);
        let holes: Vec<usize> = table.relocs.iter().map(|reloc| reloc.at).collect();
        assert_eq!(holes, [0x1c, 0x20]);
        assert_eq!(table.bytes[0x28..0x30], 1u64.to_le_bytes());
    }

    #[test]
    fn a_frame_rule_outside_a_function_or_a_function_never_ended_is_refused() {
        let why = refused("\t.cfi_def_cfa_offset 16\n");
        assert!(why.why.contains("outside"), "{why}");
        let why = refused("\t.cfi_startproc\n\tret\n");
        assert!(why.why.contains("never ended"), "{why}");
    }

    /// COFF's own section letters, and the two operands after them that make a section a COMDAT.
    /// Two sections of one name about two symbols stay two sections, and `.drectve,"yni"` is the
    /// linker's options, neither read nor written by the program.
    #[test]
    fn coff_sections_take_their_own_letters_and_comdats() {
        let text = "\t.section\t.text,\"xr\",one_only,f
f:
\tret
\t.section\t.text,\"xr\",one_only,g
g:
\tret
\t.section\t.rdata$.refptr.x,\"dr\",discard,.refptr.x
.refptr.x:
\t.quad\tx
\t.section\t.drectve,\"yni\"
\t.ascii\t\" -exclude-symbols:f\"
";
        let done = match read_as(text, Arch::X86_64, ObjectFormat::Coff) {
            Ok(done) => done,
            Err(trouble) => panic!("line {}: {}", trouble.line, trouble.why),
        };
        let parts: Vec<_> = done
            .parts
            .iter()
            .map(|part| {
                (part.name.as_str(), part.group.as_ref().map(|group| group.symbol.as_str()))
            })
            .collect();
        assert!(parts.contains(&(".text", Some("f"))), "{parts:?}");
        assert!(parts.contains(&(".text", Some("g"))), "{parts:?}");
        assert!(parts.contains(&(".rdata$.refptr.x", Some(".refptr.x"))), "{parts:?}");
        let refptr = done.parts.iter().find(|part| part.name == ".rdata$.refptr.x").unwrap();
        assert_eq!(refptr.group.as_ref().unwrap().keep, Keep::Any);
        // IMAGE_SCN_CNT_INITIALIZED_DATA and IMAGE_SCN_MEM_READ.
        assert_eq!(refptr.shape.coff, 0x40 | 0x4000_0000);
        let drectve = done.parts.iter().find(|part| part.name == ".drectve").unwrap();
        assert_eq!(drectve.bytes, b" -exclude-symbols:f");
        // IMAGE_SCN_LNK_REMOVE and IMAGE_SCN_LNK_INFO.
        assert_eq!(drectve.shape.coff, 0x800 | 0x200);
        let why = match read_as("\t.section .x,\"q\"\n", Arch::X86_64, ObjectFormat::Coff) {
            Ok(_) => panic!("'q' is not a letter"),
            Err(trouble) => trouble.why,
        };
        assert!(why.contains("'q'"), "{why}");
    }

    /// gcc's spelling of a COMDAT, `.linkonce` after the `.section`, makes the same group as
    /// clang's, about the first symbol the section defines, and a group about nothing is refused.
    #[test]
    fn a_linkonce_section_is_a_comdat_about_its_first_symbol() {
        let text = "\t.section\t.rdata$.refptr.x,\"dr\"\n\t.linkonce\tdiscard\n\t.globl\t.refptr.x\n\
                    .refptr.x:\n\t.quad\tx\n\t.section\t.text$f,\"xr\"\n\t.linkonce\tsame_size\nf:\n\tret\n";
        let done = match read_as(text, Arch::X86_64, ObjectFormat::Coff) {
            Ok(done) => done,
            Err(trouble) => panic!("line {}: {}", trouble.line, trouble.why),
        };
        let refptr = done.parts.iter().find(|part| part.name == ".rdata$.refptr.x").unwrap();
        let group = refptr.group.as_ref().unwrap();
        assert_eq!((group.symbol.as_str(), group.keep), (".refptr.x", Keep::Any));
        let text = done.parts.iter().find(|part| part.name == ".text$f").unwrap();
        let group = text.group.as_ref().unwrap();
        assert_eq!((group.symbol.as_str(), group.keep), ("f", Keep::SameSize));
        let why = match read_as(
            "\t.section .y,\"dr\"\n\t.linkonce\n\t.quad 1\n",
            Arch::X86_64,
            ObjectFormat::Coff,
        ) {
            Ok(_) => panic!("a group about nothing"),
            Err(trouble) => trouble.why,
        };
        assert!(why.contains("defines no symbol"), "{why}");
    }

    /// What gcc writes for `stdcall` and `fastcall` on i686 Windows, where the `@` is part of the
    /// name: a definition, a call, a load through a DLL's pointer, an address in data and a
    /// COMDAT named after one. A string keeps its `@` as a byte.
    #[test]
    fn an_at_sign_on_i386_coff_is_part_of_the_name() {
        let text = "\t.section\t.text$_f@4,\"xr\"\n\t.linkonce\tdiscard\n\t.globl\t_f@4\n\
                    _f@4:\n\tret\t$4\n\t.text\n\t.globl\t@g@8\n@g@8:\n\tcall\t_h@12\n\
                    \tmovl\t__imp__Sleep@4, %eax\n\tpushl\t$_f@4\n\tret\t$8\n\
                    \t.data\n\t.long\t@g@8\n\t.ascii\t\"a@b\"\t# c@d\n";
        let done = match read_as(text, Arch::X86, ObjectFormat::Coff) {
            Ok(done) => done,
            Err(trouble) => panic!("line {}: {}", trouble.line, trouble.why),
        };
        let names: Vec<&str> = done.names.iter().map(|name| name.name.as_str()).collect();
        for name in ["_f@4", "@g@8", "_h@12", "__imp__Sleep@4"] {
            assert!(names.contains(&name), "{name} in {names:?}");
        }
        let comdat = done.parts.iter().find(|part| part.name == ".text$_f@4").unwrap();
        assert_eq!(comdat.group.as_ref().unwrap().symbol, "_f@4");
        let text = done.parts.iter().find(|part| part.name == ".text").unwrap();
        let called: Vec<&str> = text.relocs.iter().map(|reloc| reloc.symbol.as_str()).collect();
        assert_eq!(called, ["_h@12", "__imp__Sleep@4", "_f@4"]);
        let data = done.parts.iter().find(|part| part.name == ".data").unwrap();
        assert_eq!(data.relocs[0].symbol, "@g@8");
        assert_eq!(&data.bytes[4..], b"a@b");
        // And nowhere else, where an `@` still says how a name is reached.
        assert!(read_as("\tcall\t_h@12\n", Arch::X86, ObjectFormat::Elf).is_err());
    }

    /// How far a thread-local variable is into `.tls` on i686 Windows, which is the one `@` that
    /// is still a suffix on i386 COFF. It sits next to a decorated name and leaves it whole.
    #[test]
    fn secrel32_on_i386_coff_is_a_suffix_and_not_part_of_the_name() {
        let text = "\t.text\n\tleal\t_counter@SECREL32(%eax), %eax\n\
                    \taddl\t_start@SECREL32(%ecx), %edx\n\tcall\t_f@4\n";
        let done = match read_as(text, Arch::X86, ObjectFormat::Coff) {
            Ok(done) => done,
            Err(trouble) => panic!("line {}: {}", trouble.line, trouble.why),
        };
        let text = done.parts.iter().find(|part| part.name == ".text").unwrap();
        let named: Vec<(&str, Reference)> =
            text.relocs.iter().map(|reloc| (reloc.symbol.as_str(), reloc.kind)).collect();
        assert_eq!(named[0], ("_counter", Reference::Section));
        assert_eq!(named[1], ("_start", Reference::Section));
        assert_eq!(named[2].0, "_f@4");
        let names: Vec<&str> = done.names.iter().map(|name| name.name.as_str()).collect();
        assert!(!names.iter().any(|name| name.contains("SECREL")), "{names:?}");
        assert!(secrel("SECREL32(%eax)") && secrel("SECREL32"));
        assert!(!secrel("SECREL32x") && !secrel("4") && !secrel("SECREL"));
    }

    /// What gcc writes for a Windows function with a frame pointer, read into the same record the
    /// object writer makes for that prologue: the codes backwards with where each instruction
    /// ended, the frame register in the header, and a row in `.pdata` naming the function, its
    /// length and the record.
    #[test]
    fn a_prologue_said_with_seh_directives_is_the_windows_table() {
        let text = "\t.text\n\t.def\tf;\t.scl\t2;\t.type\t32;\t.endef\n\t.seh_proc\tf\nf:\n\
                    \tpushq\t%rbp\n\t.seh_pushreg\t%rbp\n\tpushq\t%rbx\n\t.seh_pushreg\t%rbx\n\
                    \tsubq\t$56, %rsp\n\t.seh_stackalloc\t56\n\
                    \tmovaps\t%xmm6, 32(%rsp)\n\t.seh_savexmm\t%xmm6, 32\n\
                    \tmovq\t%rsp, %rbp\n\t.seh_setframe\t%rbp, 0\n\t.seh_endprologue\n\
                    \tnop\n\tmovq\t%rbp, %rsp\n\tpopq\t%rbx\n\tpopq\t%rbp\n\tret\n\t.seh_endproc\n";
        let done = match read_as(text, Arch::X86_64, ObjectFormat::Coff) {
            Ok(done) => done,
            Err(trouble) => panic!("line {}: {}", trouble.line, trouble.why),
        };
        let f = done.names.iter().find(|name| name.name == "f").unwrap();
        assert_eq!(f.sort, Sort::Func);
        let xdata = done.parts.iter().find(|part| part.name == ".xdata").unwrap();
        // pushq %rbp ends at 1, pushq %rbx at 2, subq at 6, movaps at 11 and movq at 14.
        assert_eq!(
            xdata.bytes,
            [1, 14, 6, 0x05, 14, 0x03, 11, 0x68, 2, 0, 6, 0x62, 2, 0x30, 1, 0x50]
        );
        let pdata = done.parts.iter().find(|part| part.name == ".pdata").unwrap();
        assert_eq!(pdata.bytes.len(), 12);
        let len = done.parts.iter().find(|part| part.name == ".text").unwrap().bytes.len() as i64;
        let relocs: Vec<(&str, i64)> =
            pdata.relocs.iter().map(|reloc| (reloc.symbol.as_str(), reloc.addend)).collect();
        assert_eq!(relocs, [("f", 0), ("f", len), ("$unwind$f", 0)]);
        assert!(pdata.relocs.iter().all(|reloc| reloc.kind == Reference::Image));

        let why = match read_as(
            "\t.seh_proc\tf\nf:\n\t.seh_endprologue\n\tpushq\t%rbx\n\t.seh_pushreg\t%rbx\n",
            Arch::X86_64,
            ObjectFormat::Coff,
        ) {
            Ok(_) => panic!("a code after the prologue ended"),
            Err(trouble) => trouble.why,
        };
        assert!(why.contains("after '.seh_endprologue'"), "{why}");
    }

    /// What clang writes for two AArch64 Windows functions: one whose prologue is the shape the
    /// packed form rebuilds, which gets its description in the row and nothing in `.xdata`, and one
    /// that returns from two places, which gets a whole description with a scope for each epilogue
    /// pointing into the prologue's codes. The bytes are the ones clang's own assembler writes.
    #[test]
    fn a_prologue_said_with_aarch64_seh_directives_is_the_windows_table() {
        let text = "\t.text\n\t.seh_proc\tcall\ncall:\n\
                    \tstp\tx29, x30, [sp, #-16]!\n\t.seh_save_fplr_x\t16\n\
                    \tmov\tx29, sp\n\t.seh_set_fp\n\t.seh_endprologue\n\tbl\tg\n\
                    \t.seh_startepilogue\n\tldp\tx29, x30, [sp], #16\n\t.seh_save_fplr_x\t16\n\
                    \t.seh_endepilogue\n\tret\n\t.seh_endfunclet\n\t.seh_endproc\n\
                    \t.seh_proc\ttwo\ntwo:\n\
                    \tstr\tx19, [sp, #-32]!\n\t.seh_save_reg_x\tx19, 32\n\
                    \tstp\tx29, x30, [sp, #8]\n\t.seh_save_fplr\t8\n\
                    \tadd\tx29, sp, #8\n\t.seh_add_fp\t8\n\t.seh_endprologue\n\
                    \tcbz\tw0, 1f\n\
                    \t.seh_startepilogue\n\tldp\tx29, x30, [sp, #8]\n\t.seh_save_fplr\t8\n\
                    \tldr\tx19, [sp], #32\n\t.seh_save_reg_x\tx19, 32\n\t.seh_endepilogue\n\tret\n\
                    1:\tbl\tg\n\
                    \t.seh_startepilogue\n\tldp\tx29, x30, [sp, #8]\n\t.seh_save_fplr\t8\n\
                    \tldr\tx19, [sp], #32\n\t.seh_save_reg_x\tx19, 32\n\t.seh_endepilogue\n\tret\n\
                    \t.seh_endproc\n";
        let done = match read_as(text, Arch::Aarch64, ObjectFormat::Coff) {
            Ok(done) => done,
            Err(trouble) => panic!("line {}: {}", trouble.line, trouble.why),
        };
        let xdata = done.parts.iter().find(|part| part.name == ".xdata").unwrap();
        // Eleven instructions, two scopes and two words of codes, then the scopes at the fourth
        // and the eighth instruction, both starting two bytes into the codes, past the `add_fp`.
        assert_eq!(
            xdata.bytes,
            [
                0x0b, 0x00, 0x80, 0x10, 0x04, 0x00, 0x80, 0x00, 0x08, 0x00, 0x80, 0x00, 0xe2, 0x01,
                0x41, 0xd4, 0x03, 0xe4, 0xe3, 0xe3
            ]
        );
        let pdata = done.parts.iter().find(|part| part.name == ".pdata").unwrap();
        // Five instructions, chained, a frame of sixteen bytes.
        assert_eq!(pdata.bytes[4..8], 0x00e0_0015u32.to_le_bytes());
        let relocs: Vec<(&str, usize)> =
            pdata.relocs.iter().map(|reloc| (reloc.symbol.as_str(), reloc.at)).collect();
        assert_eq!(relocs, [("call", 0), ("two", 8), ("$unwind$two", 12)]);
        assert!(pdata.relocs.iter().all(|reloc| reloc.kind == Reference::Image));

        for (text, what) in [
            (
                "\t.seh_proc\tf\nf:\n\tstp\tx29, x30, [sp, #-16]!\n\tmov\tx29, sp\n\
                 \t.seh_save_fplr_x\t16\n\t.seh_endprologue\n\tret\n\t.seh_endproc\n",
                "8 bytes long with 1 unwind codes",
            ),
            (
                "\t.seh_proc\tf\nf:\n\t.seh_endprologue\n\tsub\tsp, sp, #16\n\
                 \t.seh_stackalloc\t16\n\tret\n\t.seh_endproc\n",
                "outside an epilogue",
            ),
            (
                "\t.seh_proc\tf\nf:\n\tsub\tsp, sp, #24\n\t.seh_stackalloc\t24\n\
                 \t.seh_endprologue\n\tret\n\t.seh_endproc\n",
                "not a multiple of sixteen",
            ),
        ] {
            let why = match read_as(text, Arch::Aarch64, ObjectFormat::Coff) {
                Ok(_) => panic!("{text} was read"),
                Err(trouble) => trouble.why,
            };
            assert!(why.contains(what), "{why}");
        }
    }

    /// The padding the kernel's ALTERNATIVE macro puts after the original instructions, with the
    /// replacement further down in a section of its own. When the replacement is longer the
    /// original is padded with that many `nop` bytes, which is thirteen here, and the table's
    /// distances and lengths come out as gas writes them.
    #[test]
    fn the_padding_of_an_alternative_with_a_longer_replacement_is_the_difference() {
        let out = assembled(
            "140: call foo\n141:\n\
             .skip -(((144f-143f)-(141b-140b)) > 0) * ((144f-143f)-(141b-140b)),0x90\n142:\n\
             .pushsection .altinstructions,\"a\"\n.long 140b - .\n.long 143f - .\n\
             .byte 142b-140b\n.byte 144f-143f\n.popsection\n\
             .pushsection .altinstr_replacement,\"ax\"\n\
             143: movq %gs:0x28, %rax\n movq %rax, %gs:0x30\n144:\n.popsection\nret\n",
        );
        let text = bytes(&out, ".text");
        assert_eq!(text.len(), 19, "{text:x?}");
        assert!(text[5..18].iter().all(|&byte| byte == 0x90), "{text:x?}");
        assert_eq!(text[18], 0xc3);
        let table = bytes(&out, ".altinstructions");
        assert_eq!(table[8..], [18, 18]);
    }

    /// The same padding when the replacement is shorter, which is a negative count that the
    /// comparison zeroes, so there is no padding at all.
    #[test]
    fn the_padding_of_an_alternative_with_a_shorter_replacement_is_nothing() {
        let out = assembled(
            "140: movq %gs:0x28, %rax\n movq %rax, %gs:0x30\n141:\n\
             .skip -(((144f-143f)-(141b-140b)) > 0) * ((144f-143f)-(141b-140b)),0x90\n142:\n\
             .pushsection .altinstr_replacement,\"ax\"\n143: nop\n144:\n.popsection\nret\n",
        );
        assert_eq!(bytes(&out, ".text").len(), 19);
    }

    /// An alternative whose replacement has a jump in it is padded as nothing on gas's first
    /// round, since the replacement's section is not laid out yet and its two labels are each worth
    /// how far they are into their own piece. A jump in front of an alignment is judged then, and
    /// two bytes further back the target is out of reach, so gas grows it and it stays grown though
    /// with the padding in it would reach. The jump to `elsewhere` keeps the first pass from
    /// judging the short one at all.
    #[test]
    fn a_jump_grown_on_the_first_round_stays_grown() {
        let out = assembled(
            ".skip -(((744f-743f)) > 0) * (744f-743f), 0x90
jmp 1f
.balign 64
             .skip 66, 0xcc
1: jmp elsewhere
.section .other,\"ax\"
.Lx: nop
nop
             743: jmp .Lx
744:
",
        );
        let text = bytes(&out, ".text");
        assert_eq!(text[..7], [0x90, 0x90, 0xe9, 0x7b, 0, 0, 0], "{text:x?}");
    }

    /// The operators gas has, with its precedence rather than C's: `|` binds tighter than `+`, a
    /// comparison is all ones when it holds, `!` between two numbers is or-not and `>>` shifts
    /// zeroes in.
    #[test]
    fn every_operator_gas_has_works_with_the_precedence_gas_gives_it() {
        let out = assembled(
            ".byte 1 == 1, 1 != 1, 2 < 3, 3 <= 2, 3 > 2, 2 >= 2, 1 <> 2\n\
             .byte 1 && 0, 1 || 0, !0, 7 % 3, 1 << 3, 0x80 >> 4, 5 ^ 1, ~0\n\
             .byte 1 + 2 == 3, 2 * 3 + 1, 1 | 2 + 4, 6 & 3 ! 0, 2 | 1 + 1, -1 >> 60\n",
        );
        assert_eq!(
            bytes(&out, ".text"),
            [
                0xff, 0x00, 0xff, 0x00, 0xff, 0xff, 0xff, 0x00, 0x01, 0x01, 0x01, 0x08, 0x08, 0x04,
                0xff, 0xff, 0x07, 0x07, 0xff, 0x04, 0x0f
            ]
        );
    }

    /// The kernel's `(1f - 0f) == 5`, a comparison of a distance between two labels that are both
    /// further down, is worked out once they are placed.
    #[test]
    fn a_comparison_of_labels_further_down_is_worked_out() {
        let out = assembled(
            ".byte (1f - 0f) == 5, (1f - 0f) == 4\n0: .byte 1, 2, 3, 4, 5\n1:\n\
             .equ five, 1b - 0b\n.byte five\n",
        );
        assert_eq!(bytes(&out, ".text"), [0xff, 0, 1, 2, 3, 4, 5, 5]);
    }

    /// Counts of `.fill`, `.skip` and `.org` may be distances between labels on either side, and
    /// a negative distance is no bytes, which is what gas makes of it.
    #[test]
    fn fill_skip_and_org_take_their_counts_from_labels() {
        let out = assembled(
            "0: nop\nnop\n1:\n.fill 1b - 0b, 1, 0xcc\n.org 0b + 8\n.byte 1\n\
             .fill 3f - 2f, 1, 0x90\n2: .long 0\n3:\n.skip 2b - 3b\n.byte 7\n",
        );
        assert_eq!(
            bytes(&out, ".text"),
            [0x90, 0x90, 0xcc, 0xcc, 0, 0, 0, 0, 1, 0x90, 0x90, 0x90, 0x90, 0, 0, 0, 0, 7]
        );
    }

    /// `. = there` moves the place on as `.org` does, which is how the kernel lays out its kexec
    /// exception vectors, and makes no symbol called `.`.
    #[test]
    fn setting_the_place_is_an_org() {
        let out = assembled("v: nop\n . = v + 4\n.byte 1\n");
        assert_eq!(bytes(&out, ".text"), [0x90, 0, 0, 0, 1]);
        assert!(out.names.iter().all(|name| name.name != "."));
    }

    /// Eight bytes of distance to a place in another section, the way the kernel's jump table
    /// writes `.quad key - .`, is a relocation that counts from where it is.
    #[test]
    fn eight_bytes_of_distance_to_another_section_is_a_wide_relocation() {
        let out = assembled("\t.text\nx: nop\n\t.data\n\t.quad x - .\n\t.quad sym - .\n");
        let data = out.parts.iter().find(|part| part.name == ".data").unwrap();
        let kinds: Vec<_> = data.relocs.iter().map(|reloc| (reloc.at, reloc.kind)).collect();
        assert_eq!(kinds, [(0, Reference::AwayWide), (8, Reference::AwayWide)]);
    }

    /// `movq sym, %rax` with no `%rip` and `pushq $sym`, both of which the kernel writes, are a
    /// sign extended address, and the bytes the linker writes over are zero as gas leaves them.
    #[test]
    fn an_absolute_address_and_a_pushed_one_are_sign_extended_and_zero() {
        let out = assembled("\tmovq sym, %rax\n\tpushq $sym\n");
        let text = out.parts.iter().find(|part| part.name == ".text").unwrap();
        assert_eq!(text.bytes, [0x48, 0x8b, 0x04, 0x25, 0, 0, 0, 0, 0x68, 0, 0, 0, 0]);
        let kinds: Vec<_> = text.relocs.iter().map(|reloc| (reloc.at, reloc.kind)).collect();
        assert_eq!(kinds, [(4, Reference::Signed), (9, Reference::Signed)]);
    }

    /// A name in a count that is never placed is refused, and so is a count that feeds on itself
    /// or a layout that never settles, rather than read forever.
    #[test]
    fn a_count_that_cannot_be_worked_out_is_refused() {
        let why = refused("0: .skip sym - 0b\n");
        assert!(why.why.contains("'sym' is not a place"), "{why}");
        let why = refused("0: nop\n.fill (1f - 0b) * 2, 1, 0xcc\n1: nop\n");
        assert!(why.why.contains("depends on itself"), "{why}");
        let why = refused("0: nop\n.org 1f\n.byte 1\n1:\n");
        assert!(why.why.contains("still move"), "{why}");
    }

    /// A prefix written on the same line as its instruction goes in among the prefixes the
    /// instruction already has where gas puts it: a segment first, then the operand size, then a
    /// repeat, then `lock`, then REX. The bytes are the ones gas and llvm-mc write, except `rep
    /// insw`, whose two prefixes llvm-mc writes the other way round from gas. This follows gas. A
    /// prefix on a line of its own is the byte and nothing else.
    #[test]
    fn a_prefix_on_the_same_line_goes_where_gas_puts_it() {
        let text = "\t.text
\tlock incl (%rax)
\tlock incl %gs:(%rax)
\tlock addq $1, %gs:8(%rax)
\tlock cmpxchgq %rcx, (%rdx)
\tcs call *%rax
\tds jmp *%rax
\trep ret
\txacquire lock incl (%rax)
\trep insb
\trep insw
\trep outsl
\trep lodsb
\trep lodsq
\tlock
\tincl (%rax)
";
        let done = assembled(text);
        let code = done.parts.iter().find(|part| part.name == ".text").expect("one");
        let expected: &[u8] = &[
            0xf0, 0xff, 0x00, // lock incl (%rax)
            0x65, 0xf0, 0xff, 0x00, // lock incl %gs:(%rax)
            0x65, 0xf0, 0x48, 0x83, 0x40, 0x08, 0x01, // lock addq $1, %gs:8(%rax)
            0xf0, 0x48, 0x0f, 0xb1, 0x0a, // lock cmpxchgq %rcx, (%rdx)
            0x2e, 0xff, 0xd0, // cs call *%rax
            0x3e, 0xff, 0xe0, // ds jmp *%rax
            0xf3, 0xc3, // rep ret
            0xf2, 0xf0, 0xff, 0x00, // xacquire lock incl (%rax)
            0xf3, 0x6c, // rep insb
            0x66, 0xf3, 0x6d, // rep insw
            0xf3, 0x6f, // rep outsl
            0xf3, 0xac, // rep lodsb
            0xf3, 0x48, 0xad, // rep lodsq
            0xf0, 0xff, 0x00, // lock, then incl (%rax)
        ];
        assert_eq!(code.bytes, expected);
        let Err(trouble) = read("\tlock lock incl (%rax)\n", Arch::X86_64) else { panic!("read") };
        assert!(trouble.why.contains("same kind"), "{}", trouble.why);
    }

    #[test]
    fn a_name_set_to_a_register_is_that_register_until_it_is_set_again() {
        // How the kernel's SHA code names its registers and rotates them round the rounds.
        let aliased = assembled(
            "X0 = %xmm4\nX1 = %xmm5\n.set KEY, %rdi\nmovdqa X0, X1\nmovq 8(KEY), %rax\n\
             TMP = X0\nX0 = X1\nX1 = TMP\nmovdqa X0, X1\nKEY = 16\nmovq KEY(%rsi), %rax\n",
        );
        let plain = assembled(
            "movdqa %xmm4, %xmm5\nmovq 8(%rdi), %rax\nmovdqa %xmm5, %xmm4\nmovq 16(%rsi), %rax\n",
        );
        assert_eq!(bytes(&aliased, ".text"), bytes(&plain, ".text"));
    }

    #[test]
    fn a_name_set_to_a_number_is_that_number_in_an_operand() {
        let set = assembled(
            "frame_W = 0\nt = 2\nmovdqa %xmm0, 8*t+frame_W(%rsp)\nVL = 64\n\
             lea (512 + (16 * 16))-VL(%rdi), %rsi\nsub $VL, %rsi\n",
        );
        let plain = assembled("movdqa %xmm0, 16(%rsp)\nlea 704(%rdi), %rsi\nsub $64, %rsi\n");
        assert_eq!(bytes(&set, ".text"), bytes(&plain, ".text"));
    }

    /// gcc's listing of a `static` function with `target_clones` puts the resolver, the callers
    /// and the ifunc's `.set` in one section, all local. Any other name would then be a distance
    /// worked out here, and a call would run the resolver. Every reference to an ifunc stays a
    /// relocation against its name instead, a jump included, which is what gas writes.
    #[test]
    fn every_reference_to_an_ifunc_is_left_to_the_linker() {
        let out = assembled(
            ".text\nf.resolver:\n\tret\ncaller:\n\tcall f\n\tjmp f\n\tleaq f(%rip), %rax\n\
             .data\n\t.quad f\n.type f, @gnu_indirect_function\n.set f,f.resolver\n",
        );
        assert_eq!(name(&out, "f").sort, Sort::Ifunc);
        assert_eq!(name(&out, "f").binding, Binding::Local);
        let named = out.parts.iter().flat_map(|part| &part.relocs).filter(|r| r.symbol == "f");
        assert_eq!(named.count(), 4);
    }

    #[test]
    fn a_type_written_with_a_space_and_an_extern_are_read() {
        let out = assembled(".extern g\n.globl f\n.type f STT_FUNC\nf:\n\tret\n");
        assert_eq!(name(&out, "f").sort, Sort::Func);
    }

    /// An i386 file, read, with a failure reported as a panic naming the line it was on.
    fn i386(text: &str) -> Assembled {
        match read(text, Arch::X86) {
            Ok(assembled) => assembled,
            Err(trouble) => panic!("line {}: {}", trouble.line, trouble.why),
        }
    }

    /// The relocations of a section, as where, against what, which and with what added.
    fn relocs<'a>(assembled: &'a Assembled, name: &str) -> Vec<(usize, &'a str, Reference, i64)> {
        let part = assembled.parts.iter().find(|part| part.name == name).unwrap();
        part.relocs.iter().map(|r| (r.at, r.symbol.as_str(), r.kind, r.addend)).collect()
    }

    /// What reading an i386 file said about the line it could not read.
    fn i386_refused(text: &str) -> String {
        match read(text, Arch::X86) {
            Ok(_) => panic!("'{text}' was read and should not have been"),
            Err(trouble) => trouble.why,
        }
    }

    /// A call on i386 goes through the stub only when the file asks with `@PLT`, and is the plain
    /// distance otherwise, which is `R_386_PLT32` and `R_386_PC32` the way gas writes them.
    #[test]
    fn an_i386_call_asks_for_the_stub_only_when_it_says_plt() {
        let read = i386("\tcall puts@PLT\n\tcall puts\n\tjmp puts\n\tjmp puts@PLT\n");
        let ops: Vec<u8> = bytes(&read, ".text").chunks(5).map(|op| op[0]).collect();
        assert_eq!(ops, [0xe8, 0xe8, 0xe9, 0xe9]);
        assert_eq!(
            relocs(&read, ".text"),
            [
                (1, "puts", Reference::Call, -4),
                (6, "puts", Reference::Data, -4),
                (11, "puts", Reference::Data, -4),
                (16, "puts", Reference::Call, -4),
            ]
        );
    }

    /// Real mode code in the middle of an i386 file, the way the kernel's trampoline writes it. The
    /// bytes and the relocations are the ones gas writes for the same lines: no operand size prefix
    /// on a sixteen bit move and one on a thirty two bit one, an address in two bytes, a jump that
    /// counts two bytes to somewhere in another object, and the padding gas uses in this mode. What
    /// is added to each name goes into the bytes when the object is written, so they are zero here.
    #[test]
    fn code16_turns_the_prefixes_round_until_code32() {
        let read = i386(
            "\t.text\n\t.code16\na:\tmovw $0x1000, %ax\n\tmovl %eax, b\n\tjne a\n\tjmp away\n\
             \tlgdtl b\n\t.balign 8\nb:\t.long 0\n\t.code32\n\tmovl $1, %eax\n",
        );
        assert_eq!(
            bytes(&read, ".text"),
            [
                0xb8, 0x00, 0x10, 0x66, 0xa3, 0, 0, 0x75, 0xf7, 0xe9, 0, 0, 0x66, 0x0f, 0x01, 0x16,
                0, 0, 0x2e, 0x8d, 0xb4, 0x00, 0x00, 0x90, 0, 0, 0, 0, 0xb8, 1, 0, 0, 0
            ]
        );
        assert_eq!(
            relocs(&read, ".text"),
            [
                (5, "b", Reference::Address { bytes: 2 }, 0),
                (10, "away", Reference::Short, -2),
                (16, "b", Reference::Address { bytes: 2 }, 0),
            ]
        );
    }

    /// A distance to another section in one byte and in two, which the kernel's boot header writes
    /// for the short jump it spells out a byte at a time. gas asks for `R_X86_64_PC8` and
    /// `R_X86_64_PC16`, with the same addends as here.
    #[test]
    fn a_distance_in_one_byte_or_two_is_a_relocation_too() {
        let read = assembled("\t.text\n\t.byte f-1f\n1:\n\t.word f-.\n\t.data\nf:\n");
        assert_eq!(
            relocs(&read, ".text"),
            [(0, "f", Reference::Tiny, -1), (1, "f", Reference::Short, 0)]
        );
    }

    /// A global name in the same section is one another object may take the place of. gas leaves
    /// a call to it to the linker and works a jump to it out, unless the jump asked for the stub,
    /// in which case it is long and a relocation.
    #[test]
    fn an_i386_branch_to_a_global_here_is_worked_out_unless_it_asks_for_the_stub() {
        let read = i386(
            "\t.globl g\ng:\n\tcall g\n\tcall g@PLT\n\tjmp g\n\tje g\n\tjmp g@PLT\n\tje g@PLT\n\
             \tjmp sf@PLT\nsf:\tret\n",
        );
        let text = bytes(&read, ".text");
        assert_eq!(text[10..14], [0xeb, 0xf4, 0x74, 0xf2]);
        assert_eq!(text[14], 0xe9);
        assert_eq!(text[19..21], [0x0f, 0x84]);
        assert_eq!(text[25..27], [0xeb, 0x00]);
        assert_eq!(
            relocs(&read, ".text"),
            [
                (1, "g", Reference::Data, -4),
                (6, "g", Reference::Call, -4),
                (15, "g", Reference::Call, -4),
                (21, "g", Reference::Call, -4),
            ]
        );
    }

    /// `@GOTOFF` is how far a name is from the table and `@GOT` is a slot of it. gas asks for
    /// `R_386_GOT32X` where the linker can rewrite the load and `R_386_GOT32` everywhere else.
    #[test]
    fn i386_names_reached_from_the_global_offset_table() {
        let read = i386(concat!(
            "\tleal .LC0@GOTOFF(%ebx), %eax\n",
            "\tmovl foo@GOT(%ebx), %eax\n",
            "\tmovl %eax, foo@GOT(%ebx)\n",
            "\taddl foo@GOT(%ebx), %eax\n",
            "\ttestl %eax, foo@GOT(%ebx)\n",
            "\tpushl foo@GOT(%ebx)\n",
            "\tcall *foo@GOT(%ebx)\n",
            "\tmovw foo@GOT(%ebx), %ax\n",
            "\tmovl foo@GOT(%ebx,%ecx,4), %eax\n",
            "\tmovl foo@GOT(,%ecx,4), %eax\n",
            "\tleal foo@GOT(%ebx), %eax\n",
            "\tmovl sv@GOTOFF+4(%ebx), %eax\n",
            "\tmovl sv+8@GOTOFF(%ebx), %eax\n",
            "\tmovl $foo@GOT, %eax\n",
            "\tmovl foo@GOT, %eax\n",
            "\t.section .rodata\n",
            ".LC0:\t.string \"hi\"\n",
            "\t.data\n",
            "sv:\t.long 0\n",
        ));
        let text = bytes(&read, ".text");
        // Every one of them keeps its addressing byte, `movl foo@GOT, %eax` included.
        assert_eq!(text[..2], [0x8d, 0x83]);
        assert_eq!(text[text.len() - 6..text.len() - 4], [0x8b, 0x05]);
        assert_eq!(
            relocs(&read, ".text"),
            [
                (2, ".LC0", Reference::GotOffset, 0),
                (8, "foo", Reference::Slot, 0),
                (14, "foo", Reference::SlotKept, 0),
                (20, "foo", Reference::Slot, 0),
                (26, "foo", Reference::Slot, 0),
                (32, "foo", Reference::SlotKept, 0),
                (38, "foo", Reference::Slot, 0),
                (45, "foo", Reference::SlotKept, 0),
                (52, "foo", Reference::Slot, 0),
                (59, "foo", Reference::SlotKept, 0),
                (65, "foo", Reference::SlotKept, 0),
                (71, "sv", Reference::GotOffset, 4),
                (77, "sv", Reference::GotOffset, 8),
                (82, "foo", Reference::SlotKept, 0),
                (88, "foo", Reference::Slot, 0),
            ]
        );
    }

    /// The table's own name is the distance from the bytes to it, counted from the start of the
    /// instruction, so the bytes in front of the number are added. gcc's older `+[.-.L1]` moves
    /// that start back to the label the address was popped at.
    #[test]
    fn the_global_offset_table_on_i386_is_counted_from_the_start_of_the_instruction() {
        let read = i386(concat!(
            "\taddl $_GLOBAL_OFFSET_TABLE_, %ebx\n",
            "\tcall .L1\n",
            ".L1:\tpopl %ebx\n",
            "\taddl $_GLOBAL_OFFSET_TABLE_+[.-.L1], %ebx\n",
            "\tmovl $_GLOBAL_OFFSET_TABLE_+4, %eax\n",
            "\tleal _GLOBAL_OFFSET_TABLE_(%ebx), %eax\n",
            "\t.data\n",
            "\t.long _GLOBAL_OFFSET_TABLE_\n",
            "\t.long _GLOBAL_OFFSET_TABLE_-.\n",
            "\t.long _GLOBAL_OFFSET_TABLE_+8\n",
        ));
        assert_eq!(
            relocs(&read, ".text"),
            [
                (2, "_GLOBAL_OFFSET_TABLE_", Reference::GotFront, 2),
                (14, "_GLOBAL_OFFSET_TABLE_", Reference::GotFront, 3),
                (19, "_GLOBAL_OFFSET_TABLE_", Reference::GotFront, 5),
                (25, "_GLOBAL_OFFSET_TABLE_", Reference::GotFront, 2),
            ]
        );
        assert_eq!(
            relocs(&read, ".data"),
            [
                (0, "_GLOBAL_OFFSET_TABLE_", Reference::GotFront, 0),
                (4, "_GLOBAL_OFFSET_TABLE_", Reference::GotFront, 0),
                (8, "_GLOBAL_OFFSET_TABLE_", Reference::GotFront, 8),
            ]
        );
    }

    /// gas pads each record in `.eh_frame` to four bytes and only the last one to eight, so two
    /// short functions take sixty four bytes rather than seventy two. The bytes are the ones gas
    /// 2.44 writes for the vdso's `vfutex.c`.
    #[test]
    fn frame_records_are_padded_to_four_bytes_and_the_last_to_eight() {
        let out = assembled(concat!(
            "f:\n\t.cfi_startproc\n\tmovl %esi, %eax\n\tret\n\t.cfi_endproc\n",
            "g:\n\t.cfi_startproc\n\tmovl %esi, %eax\n\tret\n\t.cfi_endproc\n",
        ));
        #[rustfmt::skip]
        let gas = [
            0x14, 0, 0, 0, 0, 0, 0, 0, 0x01, 0x7a, 0x52, 0x00, 0x01, 0x78, 0x10, 0x01,
            0x1b, 0x0c, 0x07, 0x08, 0x90, 0x01, 0, 0, 0x10, 0, 0, 0, 0x1c, 0, 0, 0,
            0, 0, 0, 0, 0x03, 0, 0, 0, 0, 0, 0, 0, 0x10, 0, 0, 0,
            0x30, 0, 0, 0, 0, 0, 0, 0, 0x03, 0, 0, 0, 0, 0, 0, 0,
        ];
        assert_eq!(bytes(&out, ".eh_frame"), gas);
    }

    /// The call frame table on i386 counts in words of four, keeps the return address in column
    /// eight, and numbers the registers the way i386 does, which puts `%ebp` at five and `%ebx` at
    /// three. The bytes are the ones gas 2.42 writes for the same lines.
    #[test]
    fn an_i386_frame_table_is_the_one_gas_writes() {
        let read = i386(concat!(
            "f:\n\t.cfi_startproc\n\tpushl %ebp\n\t.cfi_def_cfa_offset 8\n",
            "\t.cfi_offset %ebp, -8\n\tmovl %esp, %ebp\n\t.cfi_def_cfa_register %ebp\n",
            "\tpushl %ebx\n\t.cfi_offset %ebx, -12\n\tpopl %ebx\n\t.cfi_restore %ebx\n",
            "\tpopl %ebp\n\t.cfi_def_cfa %esp, 4\n\tret\n\t.cfi_endproc\n",
        ));
        #[rustfmt::skip]
        let gas = [
            0x14, 0, 0, 0, 0, 0, 0, 0, 0x01, 0x7a, 0x52, 0x00, 0x01, 0x7c, 0x08, 0x01,
            0x1b, 0x0c, 0x04, 0x04, 0x88, 0x01, 0, 0, 0x20, 0, 0, 0, 0x1c, 0, 0, 0,
            0, 0, 0, 0, 0x07, 0, 0, 0, 0x00, 0x41, 0x0e, 0x08, 0x85, 0x02, 0x42, 0x0d,
            0x05, 0x41, 0x83, 0x03, 0x41, 0xc3, 0x41, 0x0c, 0x04, 0x04, 0, 0,
        ];
        assert_eq!(bytes(&read, ".eh_frame"), gas);
        let table = read.parts.iter().find(|part| part.name == ".eh_frame").expect("one");
        let kinds: Vec<_> = table.relocs.iter().map(|reloc| (reloc.at, reloc.kind)).collect();
        assert_eq!(kinds, [(32, Reference::Data)]);
        assert_eq!(table.align, 4);
        assert!(i386_refused("f:\n\t.cfi_startproc\n\t.cfi_offset %rbp, -8\n").contains("rbp"));
        assert!(i386_refused("f:\n\t.cfi_startproc\n\t.cfi_offset %ebp, -6\n").contains("slot"));
    }

    /// The registers a signal frame says are in the `sigcontext`, which is above the end of the
    /// frame and has the segment registers and the flags in it as well. The vdso's i386 signal
    /// return says so for every register. The bytes are the ones gas 2.42 writes.
    #[test]
    fn a_signal_frame_names_segment_registers_saved_above_the_frame() {
        let read = i386(concat!(
            "f:\n\t.cfi_startproc\n\tnop\n\t.cfi_offset es, 8\n\t.cfi_offset eflags, 64\n",
            "\t.cfi_offset %ds, -8\n\tud2a\n\t.cfi_endproc\n",
        ));
        #[rustfmt::skip]
        let gas = [
            0x14, 0, 0, 0, 0, 0, 0, 0, 0x01, 0x7a, 0x52, 0x00, 0x01, 0x7c, 0x08, 0x01,
            0x1b, 0x0c, 0x04, 0x04, 0x88, 0x01, 0, 0, 0x18, 0, 0, 0, 0x1c, 0, 0, 0,
            0, 0, 0, 0, 0x03, 0, 0, 0, 0x00, 0x41, 0x11, 0x28, 0x7e, 0x11, 0x09, 0x70,
            0xab, 0x02, 0, 0,
        ];
        assert_eq!(bytes(&read, ".eh_frame"), gas);
        assert_eq!(bytes(&read, ".text"), [0x90, 0x0f, 0x0b]);
    }

    /// A signal frame's rules go in a header of its own, marked `S`, with no state a call leaves
    /// behind when it said `simple`, and a function after it gets a plain header written just in
    /// front of its record. The bytes are what gas 2.42 writes, but for the one that says where
    /// `g` is, which the object writer fills in from the relocation.
    #[test]
    fn a_signal_frame_gets_a_header_of_its_own_holding_its_rules() {
        let read = i386(concat!(
            "f:\n\t.cfi_startproc simple\n\t.cfi_signal_frame\n\t.cfi_def_cfa esp, 4\n",
            "\t.cfi_offset eip, 8\n\tnop\n\t.cfi_endproc\n",
            "g:\n\t.cfi_startproc\n\tnop\n\t.cfi_def_cfa_offset 8\n\tnop\n\t.cfi_endproc\n",
        ));
        #[rustfmt::skip]
        let gas = [
            0x14, 0, 0, 0, 0, 0, 0, 0, 0x01, 0x7a, 0x52, 0x53, 0x00, 0x01, 0x7c, 0x08,
            0x01, 0x1b, 0x0c, 0x04, 0x04, 0x11, 0x08, 0x7e, 0x10, 0, 0, 0, 0x1c, 0, 0, 0,
            0, 0, 0, 0, 0x01, 0, 0, 0, 0, 0, 0, 0, 0x14, 0, 0, 0,
            0, 0, 0, 0, 0x01, 0x7a, 0x52, 0x00, 0x01, 0x7c, 0x08, 0x01, 0x1b, 0x0c, 0x04, 0x04,
            0x88, 0x01, 0, 0, 0x10, 0, 0, 0, 0x1c, 0, 0, 0, 0, 0, 0, 0,
            0x02, 0, 0, 0, 0x00, 0x41, 0x0e, 0x08,
        ];
        assert_eq!(bytes(&read, ".eh_frame"), gas);
    }

    /// A distance between two labels already down in one piece of the section is a number by the
    /// time the line is read, so it gets the short form, and one across a jump that may grow or an
    /// alignment, or to a label further down, is left to the end and gets the long one. The bytes
    /// are what gas 2.42 writes.
    #[test]
    fn a_distance_already_known_is_a_short_immediate() {
        let read = i386(concat!(
            "s:\tnop\nx:\tsubl $(x - s), %ebp\n\tjmp foo\ny:\tsubl $(y - s), %ebp\n",
            "\tsubl $(x - s), %ebp\n\t.balign 8\nz:\tsubl $(z - y), %ebp\n",
            "\tsubl $(w - z), %ebp\nw:\tpushl $(w - z)\n\tsubl $(. - z), %eax\n",
            "\tandl $(w - z + 4), %ecx\nfoo:\tret\n",
        ));
        #[rustfmt::skip]
        let gas = [
            0x90, 0x83, 0xed, 0x01, 0xeb, 0x1e, 0x81, 0xed, 0x06, 0, 0, 0, 0x83, 0xed, 0x01, 0x90,
            0x81, 0xed, 0x0a, 0, 0, 0, 0x81, 0xed, 0x0c, 0, 0, 0, 0x6a, 0x0c, 0x83, 0xe8,
            0x0e, 0x83, 0xe1, 0x10, 0xc3,
        ];
        assert_eq!(bytes(&read, ".text"), gas);
    }

    /// The i386 thread-local suffixes, in either case, in an address, a number and a data
    /// directive, with the bytes and relocations gas 2.42 writes for them. Whatever is added goes
    /// in the relocation and the name is kept even when it is local, the way gas keeps it.
    #[test]
    fn the_i386_thread_local_suffixes_are_the_ones_gas_writes() {
        let read = i386(concat!(
            "f:\tmovl %gs:0, %eax\n",
            "\tmovl %gs:x@ntpoff, %eax\n",
            "\tmovl %gs:x@NTPOFF+4, %ecx\n",
            "\tleal x@ntpoff(%eax), %eax\n",
            "\taddl $x@ntpoff, %eax\n",
            "\tmovl ex@gotntpoff(%ebx), %eax\n",
            "\tmovl ex@indntpoff, %eax\n",
            "\tmovl ex@indntpoff, %ecx\n",
            "\tsubl ex@gottpoff(%ebx), %eax\n",
            "\tmovl $x@tpoff, %eax\n",
            "\tleal ex@tlsgd(,%ebx,1), %eax\n",
            "\tcall ___tls_get_addr@PLT\n",
            "\tleal x@tlsldm(%ebx), %eax\n",
            "\tmovl x@dtpoff+4(%eax), %edx\n",
            "\t.section .tbss,\"awT\",@nobits\n",
            "\t.type x, @object\n",
            "x:\t.zero 8\n",
            "\t.section .rodata\n",
            "\t.long x@dtpoff, x@tpoff, x@ntpoff, ex@tpoff+4\n",
        ));
        #[rustfmt::skip]
        let gas: [u8; 82] = [
            0x65, 0xa1, 0, 0, 0, 0,
            0x65, 0xa1, 0, 0, 0, 0,
            0x65, 0x8b, 0x0d, 0, 0, 0, 0,
            0x8d, 0x80, 0, 0, 0, 0,
            0x05, 0, 0, 0, 0,
            0x8b, 0x83, 0, 0, 0, 0,
            0xa1, 0, 0, 0, 0,
            0x8b, 0x0d, 0, 0, 0, 0,
            0x2b, 0x83, 0, 0, 0, 0,
            0xb8, 0, 0, 0, 0,
            0x8d, 0x04, 0x1d, 0, 0, 0, 0,
            0xe8, 0, 0, 0, 0,
            0x8d, 0x83, 0, 0, 0, 0,
            0x8b, 0x90, 0, 0, 0, 0,
        ];
        assert_eq!(bytes(&read, ".text"), gas);
        assert_eq!(
            relocs(&read, ".text"),
            [
                (8, "x", Reference::Tls(Tls::Offset), 0),
                (15, "x", Reference::Tls(Tls::Offset), 4),
                (21, "x", Reference::Tls(Tls::Offset), 0),
                (26, "x", Reference::Tls(Tls::Offset), 0),
                (32, "ex", Reference::Tls(Tls::Slot), 0),
                (37, "ex", Reference::Tls(Tls::SlotAddress), 0),
                (43, "ex", Reference::Tls(Tls::SlotAddress), 0),
                (49, "ex", Reference::Tls(Tls::SlotNegated), 0),
                (54, "x", Reference::Tls(Tls::Negated), 0),
                (61, "ex", Reference::Tls(Tls::General), 0),
                (66, "___tls_get_addr", Reference::Call, -4),
                (72, "x", Reference::Tls(Tls::Module), 0),
                (78, "x", Reference::Tls(Tls::InModule), 4),
            ]
        );
        assert_eq!(
            relocs(&read, ".rodata"),
            [
                (0, "x", Reference::Tls(Tls::InModule), 0),
                (4, "x", Reference::Tls(Tls::Negated), 0),
                (8, "x", Reference::Tls(Tls::Offset), 0),
                (12, "ex", Reference::Tls(Tls::Negated), 4),
            ]
        );
        assert_eq!(name(&read, "x").sort, Sort::Thread);
        assert_eq!(name(&read, "ex").sort, Sort::Thread);
        assert!(i386_refused("\t.quad x@dtpoff\n").contains("four"));
        assert!(i386_refused("\tmovl $5@ntpoff, %eax\n").contains("thread"));
    }

    /// An address with no registers in it is four bytes the linker fills with the address, and a
    /// `mov` of one into or out of the accumulator takes the form with no addressing byte.
    #[test]
    fn an_i386_address_of_a_name_is_four_bytes_of_it() {
        let read = i386(concat!(
            "\tmovl $foo, %eax\n",
            "\tpushl $foo\n",
            "\tmovl foo, %eax\n",
            "\tmovl foo+8, %ecx\n",
            "\tmovl %eax, foo\n",
            "\tmovb foo, %al\n",
            "\tmovl bar(,%eax,4), %eax\n",
            "\tmovl foo@GOTOFF, %eax\n",
            "\t.data\n",
            "\t.long foo\n",
            "bar:\t.long 0\n",
        ));
        let text = bytes(&read, ".text");
        assert_eq!(
            [text[0], text[5], text[10], text[15], text[16], text[21], text[26], text[38]],
            [0xb8, 0x68, 0xa1, 0x8b, 0x0d, 0xa3, 0xa0, 0xa1]
        );
        let text = relocs(&read, ".text");
        let at: Vec<usize> = text.iter().map(|reloc| reloc.0).collect();
        assert_eq!(at, [1, 6, 11, 17, 22, 27, 34, 39]);
        assert_eq!(text[3].3, 8);
        assert_eq!(text[6].1, "bar");
        assert_eq!(text[7].2, Reference::GotOffset);
        assert_eq!(relocs(&read, ".data")[0].1, "foo");
    }

    /// gcc fills a jump table with how far each case is from the global offset table.
    #[test]
    fn an_i386_directive_holds_a_distance_from_the_table_or_a_slot_of_it() {
        let read = i386(
            "\t.text\n.L1:\tret\n\t.data\n\t.long .L1@GOTOFF\n\t.long foo@GOT\n\t.long foo@GOTOFF+4\n",
        );
        assert_eq!(
            relocs(&read, ".data"),
            [
                (0, ".L1", Reference::GotOffset, 0),
                (4, "foo", Reference::SlotKept, 0),
                (8, "foo", Reference::GotOffset, 4),
            ]
        );
    }

    /// What i386 has no relocation for is refused rather than written as something else.
    #[test]
    fn i386_forms_that_are_not_read_yet_are_refused() {
        assert!(i386_refused("\tleal foo@TLSDESC(%ebx), %eax\n").contains("@TLSDESC"));
        assert!(i386_refused("\tmovl foo@GOTPCREL(%rip), %eax\n").contains("i386"));
        assert!(i386_refused("\tmovl (%rax), %eax\n").contains("sixty four"));
        assert!(i386_refused("\t.quad foo@GOTOFF\n").contains("four"));
        assert!(i386_refused("\tcall foo@GOT\n").contains("@GOT"));
        assert!(i386_refused("\tmovw $foo@GOTOFF, %ax\n").contains("four"));
        assert!(i386_refused("\tjmp _GLOBAL_OFFSET_TABLE_\n").contains("table"));
    }

    /// The same lines on x86-64 are what they always were.
    #[test]
    fn x86_64_calls_are_left_as_they_were() {
        let read = assembled("\tcall puts\n\tcall puts@PLT\n\tmovl $foo, %eax\n");
        let kinds: Vec<_> =
            relocs(&read, ".text").into_iter().map(|(_, _, kind, _)| kind).collect();
        assert_eq!(kinds[..2], [Reference::Call, Reference::Call]);
    }
}
