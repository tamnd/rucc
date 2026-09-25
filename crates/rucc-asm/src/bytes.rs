//! Machine functions as the bytes of a text section.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.1. The other end of [`crate::att`], and
//! deliberately the same walk: an opcode is the list of instructions the target says it is, each
//! instruction's arguments are drawn from the operands the target says they come from, and the
//! only difference is that this hands each one to the encoder instead of writing its name. That
//! is what section 11.1 means by one description rather than two, and it is why a mistake here
//! cannot be a mistake about what an instruction is. It can only be a mistake about bytes.
//!
//! # What the encoder cannot know
//!
//! Where anything outside the instruction is. A jump carries the distance to its target and the
//! target is a block that may not have been written yet, and a call carries the distance to a
//! function that is not in this file at all. The encoder leaves four bytes for each and says
//! where it left them, and this fills in the ones it can and records the ones it cannot.
//!
//! The ones it can are the jumps inside a function, since by the end of a function every block
//! has a place. They are patched here and nothing downstream ever hears about them.
//!
//! The ones it cannot are the references to a symbol, which are a relocation: an offset into the
//! section, the name of the thing wanted, and what the linker is being asked for. Choosing which
//! relocation goes with which addressing mode is this layer's job rather than the object writer's,
//! per section 11.3, because it is a fact about the instruction and not about the file format.
//!
//! # What is not decided here
//!
//! How long a jump is. Every one of them takes four bytes for its distance whether it needs them
//! or not, which is correct and larger than it has to be. Shrinking the ones that fit in a byte is
//! relaxation, an iterate-to-fixpoint pass over the whole function, and it is not written yet.
//! Nothing here would have to change for it: it would run before this and settle the lengths.
//!
//! Alignment between functions, beyond starting each one on a sixteen byte boundary, which is what
//! every x86-64 toolchain does and what the instruction fetcher is built around. The padding is
//! written as single byte nops. A longer nop is fewer instructions to decode and the padding
//! between two functions is never executed, so there is nothing to be gained by it.

use rucc_base::Interner;
use rucc_diag::Span;
use rucc_mir::{Amode, Block, Func, Inst, Operand, Reach, defs};
use rucc_target::x86_64::{self, Addr, Arg, RAX, Value, Width};
use rucc_target::{ObjectFormat, PhysReg, TargetInfo};
use rucc_tuple::Arch;

use rucc_object::{
    Binding, Chunk, Extent, FUNC_ALIGN, Held, Marker, Patch, Reference, Reloc, Table, Text,
    Visibility,
};

use crate::Error;
use crate::format::{Directives, binding, visibility};
use crate::unwind::{self, Rows};

/// The prefix every x86-64 opcode carries in the machine IR.
const PREFIX: &str = "x64.";

/// The one byte instruction that does nothing, which is what the space in front of a function is.
///
/// Also what the room a patcher was promised is made of. The two are the same byte and not the same
/// thing: the padding is space nothing reaches, and the room is space something jumps into once it
/// has been written over. See `assemble`.
const NOP: u8 = 0x90;

/// Where one machine instruction ended up, and where in the source it came from.
///
/// The span rather than a file and a line, because this layer has no source map and no business
/// acquiring one. Turning a span into a place is the driver's, which is also where the paths a
/// `-ffile-prefix-map` rewrites are still paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    /// How far into its own function the instruction begins.
    pub at: usize,
    /// What the machine IR said this instruction was for.
    pub span: Span,
    /// Which instruction of the machine function it is, or `None` for the row the prologue gets,
    /// which is the one row here that no instruction wrote.
    ///
    /// The line table has no use for it and the locations do: a local the allocator kept in a
    /// register is somewhere over a stretch the back end named by an instruction at each end,
    /// because a machine instruction has no length until something encodes it, and this is where
    /// it gets one. Carried on the row rather than as a second list because the two are the same
    /// walk and a second list is a thing that can come to disagree with the first.
    pub inst: Option<Inst>,
}

/// A text section and, when the build asked for it, where each instruction in it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assembled {
    /// The instructions, and what the linker has to be told about them.
    pub text: Text,
    /// One list per function of [`Text::funcs`], in the same order, and empty throughout in a
    /// build that asked for no debug information.
    pub lines: Vec<Vec<Row>>,
    /// The frame rules as `.debug_frame`, in a build that asked for debug information and for no
    /// unwind table, where it is the only table a debugger has to find a frame base through. None
    /// in every other build, and on a format that has no such section.
    pub frames: Option<Chunk>,
}

/// Every function, as the bytes of a text section.
///
/// `unwind` is whether a function is described to an unwinder, which is
/// `rucc_session::Options::unwinds` and is asked of the build rather than worked out here, so that
/// this and the text writer cannot answer it differently for one function.
///
/// `lines` is whether to record where each instruction came from, which is
/// `rucc_session::Options::debug_info` and is asked the same way and for the same reason. It is a
/// question rather than something always answered because the rows are one per machine instruction
/// and a build that is not writing debug information would carry them the length of the back end to
/// throw them away.
///
/// # Errors
///
/// [`Error::Machine`] for an architecture nothing here encodes, and the rest for a function that
/// should not have got this far. See [`Error`].
///
/// # Panics
///
/// Panics on a function that was promised room for a patcher and has none on either side of its
/// own label, which is a prologue that recorded room it did not write.
pub fn assemble(
    funcs: &[Func],
    names: &Interner,
    target: &TargetInfo,
    unwind: bool,
    lines: bool,
) -> Result<Assembled, Error> {
    if target.tuple.arch() != Arch::X86_64 {
        return Err(Error::Machine { triple: target.tuple.to_string() });
    }
    let mut text = Text::default();
    let mut all = Vec::new();
    // Where each function's frame rules landed, kept beside the extents rather than written into
    // the section as they are found, because a record counts from the start of its function and the
    // function's own length is not known until its last instruction has been encoded.
    let mut rows = Vec::with_capacity(funcs.len());
    for func in funcs {
        // What this function asked for, which pads the space in front of it and, once every
        // function has been through here, is what the whole section is aligned to. Both halves
        // are needed: the offset inside the section is this padding and where the section itself
        // lands is the alignment recorded on it. It goes on the extent as well, because under
        // `-ffunction-sections` this function is a section of its own and the padding in front of
        // it is gone, so this number is the only thing left saying what it wanted.
        let align = func.align.unwrap_or(FUNC_ALIGN);
        text.align = text.align.max(align);
        let step = usize::try_from(align).unwrap_or(1).max(1);
        while text.bytes.len() % step != 0 {
            text.bytes.push(NOP);
        }
        // The half of the room a patcher was promised that is in front of the function's own
        // label, laid down here because it is the one part of a finished function that is not in a
        // block. What makes it the space in front of the function rather than the start of it is
        // everything below: the symbol, the size and the record an unwinder reads all begin after
        // it, which is what gcc does with the same flag and what a debugger showing a backtrace
        // through a patched function needs.
        //
        // The byte is written rather than encoded because the room is counted in bytes and the
        // instruction that fills it has no operands. `an_entry_promised_to_a_patcher_is_bytes_that
        // _do_nothing_on_both_sides_of_the_symbol` is what holds it to the same byte the encoder
        // writes for the half that is in a block.
        let ahead = text.bytes.len();
        if let Some(patch) = func.patch {
            text.bytes.extend(std::iter::repeat_n(NOP, patch.before as usize));
        }
        let start = text.bytes.len();
        let name = names.resolve(func.name).to_owned();
        let mut assembler = Assembler {
            names,
            directives: Directives::of(target.object_format),
            func,
            name: &name,
            text: &mut text,
            blocks: Vec::new(),
            jumps: Vec::new(),
            rows: Vec::new(),
            lines: Vec::new(),
            wants: lines,
            start,
            room: None,
            loops: Vec::new(),
            apart: target.object_format == ObjectFormat::Elf,
        };
        assembler.func()?;
        let room = assembler.room;
        rows.push(std::mem::take(&mut assembler.rows));
        all.push(std::mem::take(&mut assembler.lines));
        let len = text.bytes.len() - start;
        // Where the record points is the front of the room, which is the half in front of the
        // label in a function that has one and the first instruction of the other half otherwise.
        // The two are not one offset because a landing pad can sit between the halves.
        let patch = func.patch.map(|patch| {
            let at = if patch.before > 0 {
                ahead
            } else {
                room.expect("room that is neither in front of the label nor anywhere after it")
            };
            Patch { at, before: patch.before as usize }
        });
        text.funcs.push(Extent {
            name,
            start,
            len,
            align,
            binding: binding(func.binding),
            visibility: visibility(func.visibility),
            patch,
        });
    }
    // In whichever of the two shapes the target reads, which is what decides whether a prologue
    // this cannot describe is a refusal or is nothing at all. See [`unwind::table`].
    // Or, when there is to be no unwind table and there is to be debug information, the same rows
    // where only a debugger looks. See [`unwind::debug_frame`].
    let mut frames = None;
    if let Some(conv) = target.call_regs {
        if unwind {
            text.unwind = unwind::table(&text.funcs, &rows, conv, target.object_format)?;
        } else if lines {
            frames = unwind::debug_frame(&text.funcs, &rows, conv, target.object_format);
        }
    }
    Ok(Assembled { text, lines: all, frames })
}

/// A template kept as text, as the bytes the assembler reads out of it on its own and the places in
/// them that name something outside it, counted from the front of the template.
///
/// Read on its own when nothing in it reaches past its own text: no second section, no alignment,
/// which counts from the front of a section this is not the front of, and no name it defines, since
/// another template may be the one that jumps to it and the two are only put together in a
/// listing. A numbered label it writes and goes to itself is a place rather than a name, and the
/// reader has already turned every jump to one into a distance. What it names and does not define
/// is left for the linker, the way gas would leave it, except for a local name, which is always in
/// the same file and so is another template's. Anything else is an error with what about the text
/// it was, and the unit goes to the assembler as a listing instead.
pub(crate) fn template(
    func: &Func,
    block: Block,
    inst: Inst,
    names: &Interner,
    directives: Directives,
) -> Result<(Vec<u8>, Vec<Reloc>), String> {
    // On a format whose names carry a prefix the text and the linker spell a name differently, and
    // which one a name in the template meant is not a question this can answer.
    if !directives.symbol().is_empty() {
        return Err("names on this format carry a prefix".to_owned());
    }
    let text = crate::att::template(func, block, inst, names, directives)
        .map_err(|trouble| trouble.to_string())?;
    let read = crate::source::read(&format!("{}\n{text}", directives.text()))
        .map_err(|trouble| trouble.why)?;
    for name in &read.names {
        let outside = name.at == Held::Undefined
            && name.binding == Binding::Global
            && name.visibility == Visibility::Default
            && !name.name.starts_with(directives.local());
        if !outside {
            return Err(format!("it names '{}' in a way only the whole file can say", name.name));
        }
    }
    match read.parts.as_slice() {
        [] => Ok((Vec::new(), Vec::new())),
        [part] if part.name == ".text" && part.align <= 1 => {
            Ok((part.bytes.clone(), part.relocs.clone()))
        }
        _ => Err("it writes into a section of its own or aligns what follows".to_owned()),
    }
}

/// A jump inside a function, waiting for the block it goes to to have a place.
struct Jump {
    /// Where the four bytes the distance goes in begin.
    at: usize,
    /// Where the instruction it belongs to ends, which is what the distance is counted from.
    end: usize,
    /// The place it goes to.
    to: To,
    /// What is added to the distance, which is nothing for a jump and is the displacement for an
    /// address that names a block and has one.
    disp: i64,
}

/// A place in this function that an instruction can name: a block, or one of its jump tables.
#[derive(Clone, Copy)]
enum To {
    Block(Block),
    Table(u32),
}

/// One function being written out.
struct Assembler<'a> {
    names: &'a Interner,
    /// How the listing spells things, which a template kept as text is filled in with before it is
    /// read. See [`template`].
    directives: Directives,
    func: &'a Func,
    name: &'a str,
    text: &'a mut Text,
    /// Where each block starts, indexed by the block's own number, or [`usize::MAX`] for one that
    /// is not in the layout.
    blocks: Vec<usize>,
    jumps: Vec<Jump>,
    /// The frame rules, each with how far into this function the instruction that changed them
    /// ended.
    rows: Rows,
    /// Where each machine instruction began and what it was for, in the order they were written.
    ///
    /// Empty in a build that asked for no debug information, which is what `wants` says.
    lines: Vec<Row>,
    /// Whether to fill `lines` in at all.
    wants: bool,
    /// Where this function starts in the section, which is what those distances are counted from.
    start: usize,
    /// Where the room a patcher was promised after the label began, which is where the instruction
    /// [`rucc_mir::Patch::after`] names was encoded.
    ///
    /// [`None`] in a function that was promised none and in one whose room is all in front of the
    /// label, which is the same answer to two different questions and is why the caller decides
    /// which of them it asked. See `assemble`.
    room: Option<usize>,
    /// How long the loop each block is the head of is, indexed by the block's own number, and zero
    /// for a block that heads none. See [`loop_sizes`].
    loops: Vec<usize>,
    /// Whether the jump tables go in `.rodata` rather than after the code, which they do on ELF.
    /// See [`Self::tables`].
    apart: bool,
}

/// How long each loop in the function is, from its head to the end of the last jump back to it,
/// indexed by the head's own number and zero for a block that is not a head.
///
/// Worked out by laying the function out once with no padding and throwing the bytes away. That is
/// exact because every jump here is four bytes of distance whatever the distance is, so no
/// instruction's length depends on where it lands and padding in front of the head moves the whole
/// loop without changing its size. The cost is encoding a function twice, and only a function
/// something asked to pad a loop in pays it.
pub(crate) fn loop_sizes(
    names: &Interner,
    directives: Directives,
    func: &Func,
) -> Result<Vec<usize>, Error> {
    let mut sizes = vec![0; func.block_count()];
    if func.heads.is_empty() {
        return Ok(sizes);
    }
    let mut text = Text::default();
    let mut scratch = Assembler {
        names,
        directives,
        func,
        name: "",
        text: &mut text,
        blocks: Vec::new(),
        jumps: Vec::new(),
        rows: Vec::new(),
        lines: Vec::new(),
        wants: false,
        start: 0,
        room: None,
        loops: Vec::new(),
        apart: false,
    };
    scratch.lay()?;
    for jump in &scratch.jumps {
        let To::Block(head) = jump.to else { continue };
        let start = scratch.blocks[head.index()];
        // A jump that ends in front of the head is the way into the loop and not the way round it.
        if start == usize::MAX || jump.end <= start || !func.heads.contains(&head) {
            continue;
        }
        sizes[head.index()] = sizes[head.index()].max(jump.end - start);
    }
    Ok(sizes)
}

impl Assembler<'_> {
    /// The blocks, and then the jumps between them once every block has a place.
    fn func(&mut self) -> Result<(), Error> {
        self.loops = loop_sizes(self.names, self.directives, self.func)?;
        self.lay()?;
        let tables = self.tables()?;
        self.patch(&tables)
    }

    /// The blocks, one after another, with the jumps between them left for [`Self::patch`].
    fn lay(&mut self) -> Result<(), Error> {
        self.blocks = vec![usize::MAX; self.func.block_count()];
        // The prologue, first, because nothing in it has a span of its own. The pushes, the frame
        // and the moves that put the arguments where the body expects them came from no expression
        // in the source, so without this the front of every function is the one part of it no row
        // covers, and a program counter in there gets no answer at all rather than a slightly
        // early one. Where the function was declared is what gcc says over those bytes.
        if self.wants && !self.func.declared.is_dummy() {
            self.lines.push(Row { at: 0, span: self.func.declared, inst: None });
        }
        let end = self.func.cfi_end();
        for block in self.func.blocks() {
            // The head of a loop is padded the way the listing asks the assembler to pad it, with
            // instructions rather than single bytes, since the block in front of it may fall in.
            // The section is told for the reason an alignment instruction tells it below, since a
            // place inside a line of the section is one inside a line of memory only if the
            // section starts on one.
            let size = self.loops.get(block.index()).copied().unwrap_or(0);
            if crate::loop_room(size).is_some() {
                let count = crate::loop_padding(self.text.bytes.len(), size);
                x86_64::nops(count, &mut self.text.bytes);
                self.text.align = self.text.align.max(crate::LINE as u32);
            }
            self.blocks[block.index()] = self.text.bytes.len();
            // And the name an image knows the block by, as a symbol at the same byte. The number
            // the jumps above use is worked out here and stays here, because both ends of a jump
            // are in this section. An image is in another one, so what it holds is a relocation
            // and a relocation names a symbol, which is what this is.
            if let Some(label) = self.func.block_name(block) {
                let name = self.names.resolve(label).to_owned();
                self.text.labels.push(Marker { name, at: self.text.bytes.len() });
            }
            for inst in self.func.insts(block) {
                // Before it is encoded, because what is wanted is where it begins and after this
                // it has already been written. A landing pad is in front of it in a function that
                // has one, which is why the room is found this way rather than measured from the
                // top of the function.
                if self.func.patch.is_some_and(|patch| patch.after == Some(inst)) {
                    self.room = Some(self.text.bytes.len());
                }
                // Where it begins rather than where it ends, which is the other way round from the
                // frame rules below and for the same reason they are that way round: a debugger is
                // asking what a program counter is in the middle of, and an unwinder is asking what
                // the frame looked like at a return address.
                if self.wants {
                    let at = self.text.bytes.len() - self.start;
                    self.lines.push(Row { at, span: self.func.span(inst), inst: Some(inst) });
                }
                self.inst(block, inst)?;
                if Some(inst) == end {
                    continue;
                }
                // Where the instruction ended, because a row takes effect after the instruction
                // that changed the answer and an unwinder is looking up a return address, which is
                // the byte after a call rather than the call itself.
                let at = self.text.bytes.len() - self.start;
                self.rows.extend(self.func.cfi_after(inst).map(|op| (at, op)));
            }
        }
        Ok(())
    }

    /// Where the jumps go, now that every block and every table has a place.
    fn patch(&mut self, tables: &[usize]) -> Result<(), Error> {
        for jump in std::mem::take(&mut self.jumps) {
            let to = match jump.to {
                To::Block(block) => self.blocks[block.index()],
                To::Table(table) => tables[table as usize],
            };
            debug_assert_ne!(to, usize::MAX, "a jump to a block that was never laid out");
            let distance = i64::try_from(to).expect("a section this size") + jump.disp
                - i64::try_from(jump.end).expect("a section this size");
            let distance = i32::try_from(distance)
                .map_err(|_| Error::Distance { func: self.name.to_owned(), bytes: distance })?;
            self.text.bytes[jump.at..jump.at + 4].copy_from_slice(&distance.to_le_bytes());
        }
        Ok(())
    }

    /// The jump tables, giving back where each one starts when it is in these bytes.
    ///
    /// On ELF each goes in `.rodata`, which is where gcc and clang put one: a table is read and
    /// never run, and in the code it takes room in the lines the instruction fetcher reads and is
    /// counted as code by anything that measures a section. What goes to the writer is which block
    /// each cell names, counted from the front of the function, and the writer makes each cell a
    /// relocation, since its two ends are no longer in one section. See [`Table`].
    ///
    /// On the other formats the table stays after the last instruction, where every cell is a
    /// distance from the table to a block with both ends in this section, so the whole table is
    /// filled in here and the linker is told nothing. The cells are four bytes each and start on a
    /// four byte boundary, reached by the byte that does nothing, although nothing ever runs into
    /// it: the last instruction of a function is a return or a jump.
    fn tables(&mut self) -> Result<Vec<usize>, Error> {
        let mut starts = Vec::with_capacity(self.func.tables.len());
        if self.func.tables.is_empty() {
            return Ok(starts);
        }
        if self.apart {
            for (index, table) in self.func.tables.iter().enumerate() {
                let block =
                    self.func.block_of(table.jump).expect("a table read by a jump in no block");
                let succs = &self.func[block].succs;
                let cells = table
                    .cells
                    .iter()
                    .map(|&cell| {
                        let to = self.blocks[succs[cell as usize].block.index()];
                        debug_assert_ne!(to, usize::MAX, "a table naming a block never laid out");
                        to - self.start
                    })
                    .collect();
                let name = self.table(index);
                self.text.tables.push(Table { name, func: self.text.funcs.len(), cells });
            }
            return Ok(starts);
        }
        while self.text.bytes.len() % 4 != 0 {
            self.text.bytes.push(NOP);
        }
        for table in &self.func.tables {
            let start = self.text.bytes.len();
            starts.push(start);
            let block = self.func.block_of(table.jump).expect("a table read by a jump in no block");
            let succs = &self.func[block].succs;
            for &cell in &table.cells {
                let to = self.blocks[succs[cell as usize].block.index()];
                debug_assert_ne!(to, usize::MAX, "a table naming a block that was never laid out");
                let distance = i64::try_from(to).expect("a section this size")
                    - i64::try_from(start).expect("a section this size");
                let distance = i32::try_from(distance)
                    .map_err(|_| Error::Distance { func: self.name.to_owned(), bytes: distance })?;
                self.text.bytes.extend_from_slice(&distance.to_le_bytes());
            }
        }
        Ok(starts)
    }

    /// The name one jump table of this function goes by, which is the one the listing gives it.
    fn table(&self, index: usize) -> String {
        format!("{}{}_j{index}", self.directives.local(), self.name)
    }

    /// One instruction of the machine IR, as however many instructions of the machine it is.
    fn inst(&mut self, block: Block, inst: Inst) -> Result<(), Error> {
        let data = self.func[inst];
        let spelled = self.names.resolve(data.opcode.name());
        let opcode = spelled.strip_prefix(PREFIX).unwrap_or(spelled);
        // The one opcode that is not an instruction. Where the listing writes the assembler's own
        // directive this has to do what the assembler would have done, which is pad up to the
        // boundary with the byte that does nothing, since the gap is reached by falling into it.
        //
        // The section has to be told as well. The padding puts the next instruction at a multiple of
        // the boundary counted from the front of the section, and what makes that an address the
        // program sees is the section itself landing on one, so the boundary goes on the section's
        // alignment the way a function's own does.
        if opcode == x86_64::ALIGN {
            let bytes = data.imm.map_or(0, |imm| self.func[imm].0);
            let boundary = u32::try_from(bytes).ok().filter(|at| at.is_power_of_two());
            let Some(boundary) = boundary else {
                return Err(Error::Opcode {
                    func: self.name.to_owned(),
                    opcode: spelled.to_owned(),
                });
            };
            self.text.align = self.text.align.max(boundary);
            let step = boundary as usize;
            while self.text.bytes.len() % step != 0 {
                self.text.bytes.push(NOP);
            }
            return Ok(());
        }
        // The other one, which is the bytes a template wrote out as themselves. There is nothing to
        // encode: the program already said what the processor is to be handed, so they go down as
        // they are.
        if opcode == x86_64::LITERAL {
            let Some(imm) = data.imm else {
                return Err(Error::Opcode {
                    func: self.name.to_owned(),
                    opcode: spelled.to_owned(),
                });
            };
            let before = self.text.bytes.len();
            self.text.bytes.extend(x86_64::unpacked(self.func[imm].0));
            if self.text.bytes.len() == before {
                return Err(Error::Opcode {
                    func: self.name.to_owned(),
                    opcode: spelled.to_owned(),
                });
            }
            return Ok(());
        }
        // A template kept as text, read on its own and laid down as what it came to. One the
        // reader cannot take on its own sends the whole unit to the assembler as a listing instead
        // and never comes here, see [`crate::kept`], so a refusal here is that check and this one
        // disagreeing.
        if opcode == x86_64::TEMPLATE {
            let (bytes, relocs) =
                template(self.func, block, inst, self.names, self.directives).map_err(|why| {
                    Error::Encode { func: self.name.to_owned(), opcode: spelled.to_owned(), why }
                })?;
            let at = self.text.bytes.len();
            self.text.bytes.extend(bytes);
            self.text
                .relocs
                .extend(relocs.into_iter().map(|reloc| Reloc { at: reloc.at + at, ..reloc }));
            return Ok(());
        }
        let Some(written) = x86_64::written(opcode) else {
            return Err(Error::Opcode { func: self.name.to_owned(), opcode: spelled.to_owned() });
        };
        let operands = &self.func[data.operands];
        for machine in written {
            // What each argument turned out to be, and what the encoder has to be told about
            // afterwards for the ones that name something it cannot see.
            let mut values = Vec::with_capacity(machine.args.len());
            let mut wanted = None;
            // The other thing an address can name, which is a place in this same function and so is
            // a distance nothing outside the file has to be told about.
            let mut labelled = None;
            for arg in machine.args {
                values.push(match *arg {
                    Arg::Reg(at, width) => {
                        Value::Reg(self.phys(operands[usize::from(at)], spelled)?, width)
                    }
                    // The same thing in the other file, which the encoder has to be told apart
                    // from the one above: which file a register is in is part of which instruction
                    // it is, and the table it looks a row up in is what says so.
                    Arg::Xmm(at) => Value::Xmm(self.phys(operands[usize::from(at)], spelled)?),
                    // The two halves of one word. The encoder numbers a high byte as the low one
                    // plus four, which is the whole of the difference between them in the bytes
                    // and is also why only the first four registers have one.
                    Arg::Low(at) => {
                        Value::Reg(self.phys(operands[usize::from(at)], spelled)?, Width::Byte)
                    }
                    Arg::High(at) => Value::High(self.phys(operands[usize::from(at)], spelled)?),
                    // The only register named outright on this machine is the high half of the
                    // first one, which an eight bit remainder comes back in.
                    Arg::Named(_) => Value::High(RAX),
                    // A depth on the x87 stack, which carries nothing across because there is
                    // nothing to carry: the depth is in the opcode byte the mnemonic picks, so
                    // what the encoder needs from here is that an argument was there at all.
                    Arg::Stack(_) => Value::Stack,
                    Arg::Lit(lane) => Value::Imm(i64::from(lane)),
                    // The first operand read, which is where a call puts the address it goes
                    // through. Everything in front of it is a register the call writes.
                    Arg::Through => {
                        Value::Reg(self.phys(operands[defs(operands)], spelled)?, Width::Quad)
                    }
                    Arg::Imm => Value::Imm(data.imm.map_or(0, |imm| self.func[imm].0)),
                    Arg::Mem => {
                        let amode = data.mem.map(|mem| self.func[mem]);
                        let (addr, symbol) = self.addr(operands, amode.as_ref(), spelled)?;
                        if let Some(symbol) = symbol {
                            // A mode that reads the global offset table names the slot rather than
                            // the thing, and the four bytes are the same four bytes either way, so
                            // which relocation it is is the whole of the difference here.
                            let kind = match amode.map_or(Reach::Itself, |mem| mem.reach) {
                                Reach::Itself => Reference::Data,
                                Reach::Table => Reference::Got,
                                Reach::Thread => Reference::Thread,
                            };
                            wanted = Some((symbol, kind, i64::from(addr.disp)));
                        }
                        if let Some(block) = amode.and_then(|mem| mem.block) {
                            labelled = Some((To::Block(block), i64::from(addr.disp)));
                        }
                        if let Some(table) = amode.and_then(|mem| mem.table) {
                            // In another section, so the linker's to fill in like any symbol.
                            if self.apart && addr.rip {
                                let name = self.table(table as usize);
                                wanted = Some((name, Reference::Data, i64::from(addr.disp)));
                            } else {
                                labelled = Some((To::Table(table), i64::from(addr.disp)));
                            }
                        }
                        Value::Mem(addr)
                    }
                    Arg::Symbol => {
                        let symbol =
                            data.symbol.map(|symbol| self.names.resolve(symbol).to_owned());
                        if let Some(symbol) = symbol {
                            wanted = Some((symbol, Reference::Call, 0));
                        }
                        Value::Dest
                    }
                    // Where a conditional jump goes is the first arm, because the block layout
                    // guarantees the second is the block laid out next and is fallen into.
                    Arg::Label => Value::Dest,
                });
            }

            let start = self.text.bytes.len();
            let holes =
                x86_64::encode(machine.mnemonic, &values, &mut self.text.bytes).map_err(|why| {
                    Error::Encode {
                        func: self.name.to_owned(),
                        opcode: spelled.to_owned(),
                        why: why.to_string(),
                    }
                })?;
            let end = self.text.bytes.len();

            // A hole is either something outside the file, which is a relocation, or a block of
            // this function, which is patched once every block has a place.
            if let Some((symbol, kind, disp)) = wanted {
                let kind = match kind {
                    Reference::Got => slot(&self.text.bytes[start..end]),
                    kind => kind,
                };
                let at = match kind {
                    Reference::Call => holes.dest,
                    Reference::Data
                    | Reference::Got
                    | Reference::GotBare
                    | Reference::GotKept
                    | Reference::Thread => holes.rip,
                    // An address written into an image rather than reached by an instruction, and
                    // how far something is from the front of one, which is what a table of data
                    // holds. Nothing above produces either, because every reference an instruction
                    // makes is a distance from where the instruction ends.
                    Reference::Address { .. } | Reference::Image | Reference::Away => {
                        unreachable!("an instruction wanting an address")
                    }
                };
                let at = at.expect("an instruction naming a symbol leaves room for the distance");
                let addend = disp - i64::try_from(end - at).expect("an instruction this long");
                // How many bytes of the instruction come after the four the linker writes over,
                // which is what is left of the distance from the hole to the end of it. Already in
                // the addend and written down again because COFF wants the two apart, and there is
                // nowhere else it can be worked out: by the time a writer sees the relocation the
                // instruction it is in is bytes like any others.
                let after = u8::try_from(end - at - 4).expect("an instruction this long");
                self.text.relocs.push(Reloc { at, symbol, kind, addend, after });
                // The addend is the whole of it, so the four bytes are left as nothing, which is
                // what gas leaves. tcc's linker adds to what is there rather than writing over it,
                // and a `mov cstr_buf+8(%rip)` with the eight in both places read eight bytes
                // past the member it wanted.
                self.text.bytes[at..at + 4].fill(0);
            } else if let Some((to, disp)) = labelled {
                // The address of a label, which is the four bytes an address counted from the
                // instruction pointer leaves and is patched where a jump is patched rather than
                // written out as a relocation, since both ends of it are in this function.
                let at = holes.rip.expect("an address naming a label leaves room for the distance");
                self.jumps.push(Jump { at, end, to, disp });
            } else if let Some(at) = holes.dest {
                match self.func[block].succs.first() {
                    Some(call) => {
                        self.jumps.push(Jump { at, end, to: To::Block(call.block), disp: 0 });
                    }
                    None => debug_assert!(false, "a jump out of a block with no arms"),
                }
            }
        }
        Ok(())
    }

    /// One address, with the operands it names resolved and the symbol it names handed back.
    ///
    /// A symbol with no base and no index is reached from the instruction pointer, which is how a
    /// global is reached in position independent code and the only way this compiler reaches one.
    /// The displacement is carried to the relocation's addend, and the four bytes it would have
    /// gone in are left as nothing once the relocation is written.
    fn addr(
        &self,
        operands: &[Operand],
        amode: Option<&Amode>,
        opcode: &str,
    ) -> Result<(Addr, Option<String>), Error> {
        let Some(amode) = amode else {
            return Ok((Addr::default(), None));
        };
        let base = match amode.base {
            Some(at) => Some(self.phys(operands[usize::from(at)], opcode)?),
            None => None,
        };
        let index = match amode.index {
            Some(at) => Some(self.phys(operands[usize::from(at)], opcode)?),
            None => None,
        };
        let symbol = amode.symbol.map(|symbol| self.names.resolve(symbol).to_owned());
        // A block is reached the same way and leaves the same four bytes. What is different is who
        // fills them in, which is this file rather than the linker, and that is the caller's to
        // sort out: what it needs from here is that the address was written that way at all.
        let names = symbol.is_some() || amode.block.is_some() || amode.table.is_some();
        let rip = names && base.is_none() && index.is_none();
        let addr =
            Addr { base, index, scale: amode.scale, disp: amode.disp, rip, segment: amode.segment };
        Ok((addr, if rip { symbol } else { None }))
    }

    /// The real register one operand ended up in.
    fn phys(&self, operand: Operand, opcode: &str) -> Result<PhysReg, Error> {
        operand
            .reg
            .phys()
            .ok_or_else(|| Error::Virtual { func: self.name.to_owned(), opcode: opcode.to_owned() })
    }
}

/// Which relocation a read of a slot of the global offset table asks for, from the bytes of the
/// instruction it is in.
///
/// The linker can turn a slot back into the address itself in only a few instructions: a `mov`
/// from memory, `test`, the eight that do arithmetic from memory into a register, and a `call` or
/// `jmp` through memory, none of them behind a `0x66`. gas asks for the relocation that allows it
/// in those and the plain one everywhere else, and says whether there is a REX prefix, which is
/// what the linker needs to know to rewrite the instruction in place.
pub(crate) fn slot(bytes: &[u8]) -> Reference {
    let mut rest = bytes;
    let mut rex = false;
    while let [first, tail @ ..] = rest {
        match first {
            0x66 => return Reference::GotKept,
            0x26 | 0x2E | 0x36 | 0x3E | 0x64 | 0x65 | 0x67 | 0xF0 | 0xF2 | 0xF3 => rest = tail,
            0x40..=0x4F => {
                rex = true;
                rest = tail;
            }
            _ => break,
        }
    }
    let rewritten = match rest {
        [0x8B | 0x85, ..] => true,
        [0xFF, modrm, ..] => matches!((modrm >> 3) & 7, 2 | 4),
        [op, ..] => *op & !0x38 == 0x03,
        [] => false,
    };
    match (rewritten, rex) {
        (false, _) => Reference::GotKept,
        (true, true) => Reference::Got,
        (true, false) => Reference::GotBare,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use rucc_base::Interner;
    use rucc_mir::{BlockCall, Mem, Opcode, Reg, Table};
    use rucc_object::{Binding, Visibility};
    use rucc_target::x86_64::{GPR, RAX, RCX, RDX};
    use rucc_target::{Arch, Env, Os, Triple};

    /// A linux x86-64 target, which is the one every case here is written for.
    fn target() -> TargetInfo {
        TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu))
    }

    /// One function of one block, with those instructions in it, assembled.
    fn write(build: impl FnOnce(&mut Func, &mut Interner)) -> Text {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        build(&mut func, &mut names);
        assemble(&[func], &names, &target(), true, false)
            .expect("a function that was allocated")
            .text
    }

    /// Those bytes, as the hexadecimal a manual writes them in.
    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect::<Vec<_>>().join(" ")
    }

    /// An addition of two registers, which is the smallest instruction with operands there is.
    fn add(func: &mut Func, names: &mut Interner) {
        let block = func.create_block();
        let add = Opcode::new(names.intern("x64.add_rr_32"));
        func.build(block, add)
            .operand(Operand::write(Reg::physical(RAX), GPR))
            .operand(Operand::read(Reg::physical(RAX), GPR))
            .operand(Operand::read(Reg::physical(RCX), GPR))
            .finish();
    }

    #[test]
    fn an_instruction_is_the_bytes_the_target_says_it_is() {
        let text = write(add);
        assert_eq!(hex(&text.bytes), "01 c8");
        let f = Extent {
            name: "f".to_owned(),
            start: 0,
            len: 2,
            align: FUNC_ALIGN,
            binding: Binding::Global,
            visibility: Visibility::Default,
            patch: None,
        };
        assert_eq!(text.funcs, [f]);
        assert!(text.relocs.is_empty());
    }

    #[test]
    fn an_opcode_the_machine_has_no_single_instruction_for_is_all_the_ones_it_has() {
        let text = write(|func, names| {
            let block = func.create_block();
            let cmp = Opcode::new(names.intern("x64.cmp_set_l_64"));
            func.build(block, cmp)
                .operand(Operand::write(Reg::physical(RAX), GPR))
                .operand(Operand::read(Reg::physical(RCX), GPR))
                .operand(Operand::read(Reg::physical(RDX), GPR))
                .finish();
        });
        // The comparison at the width it was asked for and then the set, which is the same two
        // instructions the assembly path writes and is why one description rather than two.
        assert_eq!(hex(&text.bytes), "48 39 d1 0f 9c c0");
    }

    #[test]
    fn an_opcode_that_is_not_an_instruction_is_no_bytes_at_all() {
        let text = write(|func, names| {
            let block = func.create_block();
            let ret = Opcode::new(names.intern("x64.ret_val_32"));
            func.build(block, ret).operand(Operand::read(Reg::physical(RAX), GPR)).finish();
        });
        assert!(text.bytes.is_empty(), "{:?}", text.bytes);
    }

    #[test]
    fn an_alignment_is_the_bytes_between_where_it_is_and_the_boundary_it_asks_for() {
        let text = write(|func, names| {
            let block = func.create_block();
            let add = Opcode::new(names.intern("x64.add_rr_32"));
            let align = Opcode::new(names.intern("x64.align"));
            let two = |func: &mut Func| {
                func.build(block, add)
                    .operand(Operand::write(Reg::physical(RAX), GPR))
                    .operand(Operand::read(Reg::physical(RAX), GPR))
                    .operand(Operand::read(Reg::physical(RCX), GPR))
                    .finish();
            };
            two(func);
            func.build(block, align).imm(8).finish();
            two(func);
        });
        // Two bytes of addition, six of nothing, two more of addition. The padding is the one byte
        // instruction that does nothing rather than a run of zeroes, because the processor may walk
        // through it to get to what comes after, which is the whole reason a program asks.
        assert_eq!(hex(&text.bytes), "01 c8 90 90 90 90 90 90 01 c8");
        // The section has to be told as well. A function aligned to eight inside a section aligned
        // to one is aligned to eight in its own reckoning and to nothing at all in the program's.
        assert!(text.align >= 8, "{}", text.align);
    }

    /// The bytes a template wrote out itself, which go down as they are.
    ///
    /// `xgetbv` written as its three bytes, which is how every program that has one writes it,
    /// between two instructions so that what is checked is that the bytes land where the program
    /// put them and not just that they land.
    #[test]
    fn a_byte_out_of_a_template_is_that_byte_and_nothing_around_it() {
        let text = write(|func, names| {
            let block = func.create_block();
            let add = Opcode::new(names.intern("x64.add_rr_32"));
            let byte = Opcode::new(names.intern("x64.byte"));
            let two = |func: &mut Func| {
                func.build(block, add)
                    .operand(Operand::write(Reg::physical(RAX), GPR))
                    .operand(Operand::read(Reg::physical(RAX), GPR))
                    .operand(Operand::read(Reg::physical(RCX), GPR))
                    .finish();
            };
            two(func);
            let bytes = x86_64::packed(&[0x0f, 0x01, 0xd0]).expect("three bytes fit");
            func.build(block, byte).imm(bytes).finish();
            two(func);
        });
        assert_eq!(hex(&text.bytes), "01 c8 0f 01 d0 01 c8");
    }

    #[test]
    fn a_jump_inside_a_function_is_filled_in_rather_than_left_to_the_linker() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let first = func.create_block();
        let second = func.create_block();
        let add = Opcode::new(names.intern("x64.add_rr_32"));
        func.build(first, add)
            .operand(Operand::write(Reg::physical(RAX), GPR))
            .operand(Operand::read(Reg::physical(RAX), GPR))
            .operand(Operand::read(Reg::physical(RCX), GPR))
            .finish();
        let jmp = Opcode::new(names.intern("x64.jmp"));
        func.build(second, jmp).finish();
        func.succs_mut(second).push(BlockCall::to(first));

        let text = assemble(&[func], &names, &target(), true, false).expect("two blocks").text;
        // Two bytes of addition, then a jump back over itself and over them, which is seven bytes
        // backwards because a jump counts from where it ends.
        assert_eq!(hex(&text.bytes), "01 c8 e9 f9 ff ff ff");
        assert!(text.relocs.is_empty(), "a jump inside a function is not the linker's business");
    }

    #[test]
    fn the_address_of_a_label_is_filled_in_here_as_well() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let first = func.create_block();
        let second = func.create_block();
        let lea = Opcode::new(names.intern("x64.lea_64"));
        func.build(first, lea)
            .operand(Operand::write(Reg::physical(RAX), GPR))
            .mem(Mem::block(second))
            .finish();
        let jmp = Opcode::new(names.intern("x64.jmp_reg"));
        func.build(first, jmp).operand(Operand::read(Reg::physical(RAX), GPR)).finish();
        func.succs_mut(first).push(BlockCall::to(second));
        func.build(second, Opcode::new(names.intern("x64.ret"))).finish();

        let text = assemble(&[func], &names, &target(), true, false).expect("two blocks").text;
        // Seven bytes of address, two of jump, and then the block. The distance is two, because
        // the four bytes count from the end of the instruction that holds them and the jump is
        // what is in between.
        assert_eq!(hex(&text.bytes), "48 8d 05 02 00 00 00 ff e0 c3");
        assert!(text.relocs.is_empty(), "a label of this function is not the linker's business");
    }

    /// A function that jumps through a table of three cells to one of two returns.
    fn switching(names: &mut Interner) -> Func {
        let mut func = Func::new(names.intern("f"));
        let head = func.create_block();
        let first = func.create_block();
        let second = func.create_block();
        let lea = Opcode::new(names.intern("x64.lea_64"));
        func.build(head, lea)
            .operand(Operand::write(Reg::physical(RAX), GPR))
            .mem(Mem::table(0))
            .finish();
        let jmp = Opcode::new(names.intern("x64.jmp_reg"));
        let jump = func.build(head, jmp).operand(Operand::read(Reg::physical(RAX), GPR)).finish();
        func.succs_mut(head).push(BlockCall::to(first));
        func.succs_mut(head).push(BlockCall::to(second));
        func.build(first, Opcode::new(names.intern("x64.ret"))).finish();
        func.build(second, Opcode::new(names.intern("x64.ret"))).finish();
        func.tables.push(Table { jump, cells: vec![0, 1, 0] });
        func
    }

    #[test]
    fn a_jump_table_on_elf_goes_to_the_writer_with_where_each_block_is() {
        let mut names = Interner::new();
        let func = switching(&mut names);
        let text = assemble(&[func], &names, &target(), true, false).expect("a table").text;
        // Seven bytes of address, two of jump and two returns, and nothing after them: the table
        // is not in the code. The address is the linker's to fill in, counted from the end of
        // the instruction, which is four bytes past the hole.
        assert_eq!(hex(&text.bytes), "48 8d 05 00 00 00 00 ff e0 c3 c3");
        assert_eq!(
            text.relocs,
            [Reloc {
                at: 3,
                symbol: ".Lf_j0".to_owned(),
                kind: Reference::Data,
                addend: -4,
                after: 0
            }]
        );
        // The two returns are nine and ten bytes into the function.
        let table =
            rucc_object::Table { name: ".Lf_j0".to_owned(), func: 0, cells: vec![9, 10, 9] };
        assert_eq!(text.tables, [table]);
    }

    #[test]
    fn a_jump_table_on_windows_is_written_after_the_code_as_distances_from_itself() {
        let mut names = Interner::new();
        let func = switching(&mut names);
        let target = TargetInfo::new(Triple::new(Arch::X86_64, Os::Windows, Env::Gnu));
        let text = assemble(&[func], &names, &target, true, false).expect("a table").text;
        // Seven bytes of address, two of jump and two returns end at eleven, one byte that does
        // nothing brings the table to twelve, and each cell is how far back its block is from
        // there. The address counts from the end of its own instruction, so it is five.
        assert_eq!(
            hex(&text.bytes),
            "48 8d 05 05 00 00 00 ff e0 c3 c3 90 fd ff ff ff fe ff ff ff fd ff ff ff"
        );
        assert!(text.relocs.is_empty(), "a table of this function is not the linker's business");
        assert!(text.tables.is_empty(), "{:?}", text.tables);
    }

    #[test]
    fn a_call_leaves_the_linker_the_name_of_what_it_calls() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        let call = Opcode::new(names.intern("x64.call"));
        let callee = names.intern("puts");
        func.build(block, call).symbol(callee).finish();

        let text = assemble(&[func], &names, &target(), true, false).expect("a call").text;
        assert_eq!(hex(&text.bytes), "e8 00 00 00 00");
        assert_eq!(
            text.relocs,
            [Reloc {
                at: 1,
                symbol: "puts".to_owned(),
                kind: Reference::Call,
                addend: -4,
                after: 0
            }]
        );
    }

    #[test]
    fn a_global_is_a_relocation_counted_from_the_end_of_the_instruction() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        let load = Opcode::new(names.intern("x64.mov_rm_64"));
        let global = names.intern("counter");
        func.build(block, load)
            .operand(Operand::write(Reg::physical(RAX), GPR))
            .mem(Mem::of(global).plus(8))
            .finish();

        let text =
            assemble(&[func], &names, &target(), true, false).expect("a load of a global").text;
        // The four bytes are nothing, as gas leaves them, because tcc's linker adds to what is
        // there and would count the eight twice.
        assert_eq!(hex(&text.bytes), "48 8b 05 00 00 00 00");
        // Four bytes back to where the instruction ends, and then the eight the address already
        // meant. A relocation counts from where its own bytes start and an instruction counts
        // from where it ends, and the addend is what makes up the difference.
        assert_eq!(
            text.relocs,
            [Reloc {
                at: 3,
                symbol: "counter".to_owned(),
                kind: Reference::Data,
                addend: 4,
                after: 0
            }]
        );
    }

    /// The room a patcher was promised, on both sides of the symbol.
    ///
    /// What holds the two halves to the same byte. The half in front of the label is written as a
    /// byte here and the half after it is encoded from the opcode like any other instruction, so
    /// this is what would notice if the machine ever encoded one of them as something else.
    #[test]
    fn an_entry_promised_to_a_patcher_is_bytes_that_do_nothing_on_both_sides_of_the_symbol() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        let pad = Opcode::new(names.intern("x64.nop"));
        let first = func.build(block, pad).finish();
        func.build(block, pad).finish();
        add(&mut func, &mut names);
        func.patch = Some(rucc_mir::Patch { before: 3, pad, after: Some(first) });

        let text = assemble(&[func], &names, &target(), true, false)
            .expect("a function with room in it")
            .text;
        assert_eq!(hex(&text.bytes), "90 90 90 90 90 01 c8");
        let [f] = &text.funcs[..] else { panic!("one function") };
        // The symbol is after the room in front of the label and its size counts none of it, which
        // is what makes a backtrace through the function name the function rather than the room.
        assert_eq!(f.start, 3);
        assert_eq!(f.len, 4);
        // And the record points at the front of the whole thing, which here is the front of the
        // function's bytes because there is room in front of the label.
        assert_eq!(f.patch, Some(Patch { at: 0, before: 3 }));
    }

    /// The same when the room is all after the label, which is what one number asks for.
    #[test]
    fn room_that_is_all_after_the_label_is_recorded_where_it_really_starts() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        // A landing pad in front of it, which is the one thing that goes between the label and the
        // room and is why the record is not just the top of the function.
        let landing = Opcode::new(names.intern("x64.endbr64"));
        func.build(block, landing).finish();
        let pad = Opcode::new(names.intern("x64.nop"));
        let first = func.build(block, pad).finish();
        func.build(block, pad).finish();
        add(&mut func, &mut names);
        func.patch = Some(rucc_mir::Patch { before: 0, pad, after: Some(first) });

        let text = assemble(&[func], &names, &target(), true, false)
            .expect("a function with room in it")
            .text;
        assert_eq!(hex(&text.bytes), "f3 0f 1e fa 90 90 01 c8");
        let [f] = &text.funcs[..] else { panic!("one function") };
        assert_eq!(f.start, 0);
        assert_eq!(f.patch, Some(Patch { at: 4, before: 0 }));
    }

    #[test]
    fn a_global_read_out_of_the_offset_table_asks_for_the_relocation_that_names_the_slot() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        let load = Opcode::new(names.intern("x64.mov_rm_64"));
        let away = names.intern("away");
        func.build(block, load)
            .operand(Operand::write(Reg::physical(RAX), GPR))
            .mem(Mem::got(away))
            .finish();

        let text = assemble(&[func], &names, &target(), true, false)
            .expect("a load through the offset table")
            .text;
        // A `mov` with a REX prefix, which the relocation requires by name: the linker is allowed
        // to turn it back into a `lea`, and it can only do that when it knows what it is looking
        // at down to the prefix.
        assert_eq!(hex(&text.bytes), "48 8b 05 00 00 00 00");
        assert_eq!(
            text.relocs,
            [Reloc {
                at: 3,
                symbol: "away".to_owned(),
                kind: Reference::Got,
                addend: -4,
                after: 0
            }]
        );
    }

    #[test]
    fn an_address_that_names_a_register_is_not_a_relocation() {
        let text = write(|func, names| {
            let block = func.create_block();
            let lea = Opcode::new(names.intern("x64.lea_64"));
            func.build(block, lea)
                .operand(Operand::write(Reg::physical(RAX), GPR))
                .mem(
                    Mem::at(Operand::read(Reg::physical(RCX), GPR))
                        .indexed(Operand::read(Reg::physical(RDX), GPR), 4)
                        .plus(-16),
                )
                .finish();
        });
        assert_eq!(hex(&text.bytes), "48 8d 44 91 f0");
        assert!(text.relocs.is_empty());
    }

    #[test]
    fn every_function_starts_on_a_boundary_and_the_space_in_front_of_one_does_nothing() {
        let mut names = Interner::new();
        let mut first = Func::new(names.intern("f"));
        add(&mut first, &mut names);
        let mut second = Func::new(names.intern("g"));
        add(&mut second, &mut names);

        let text =
            assemble(&[first, second], &names, &target(), true, false).expect("two functions").text;
        assert_eq!(text.funcs[1].start, 16);
        assert_eq!(text.bytes.len(), 18);
        assert!(text.bytes[2..16].iter().all(|byte| *byte == NOP), "{:?}", text.bytes);
    }

    #[test]
    fn a_function_that_was_never_allocated_is_refused_rather_than_encoded_wrongly() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        let vreg = func.new_vreg(GPR);
        let neg = Opcode::new(names.intern("x64.neg_r_32"));
        func.build(block, neg).operand(Operand::write(vreg, GPR)).finish();
        let error =
            assemble(&[func], &names, &target(), true, false).expect_err("a virtual register");
        assert_eq!(
            error,
            Error::Virtual { func: "f".to_owned(), opcode: "x64.neg_r_32".to_owned() }
        );
    }

    #[test]
    fn an_opcode_the_target_does_not_describe_is_refused() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        let made_up = Opcode::new(names.intern("x64.frobnicate"));
        func.build(block, made_up).finish();
        let error =
            assemble(&[func], &names, &target(), true, false).expect_err("no such instruction");
        assert_eq!(
            error,
            Error::Opcode { func: "f".to_owned(), opcode: "x64.frobnicate".to_owned() }
        );
    }

    #[test]
    fn a_build_that_asked_for_debug_information_is_told_where_each_instruction_began() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        let add = Opcode::new(names.intern("x64.add_rr_32"));
        for at in 0..2u32 {
            func.build(block, add)
                .at(Span::new(at * 10, at * 10 + 3))
                .operand(Operand::write(Reg::physical(RAX), GPR))
                .operand(Operand::read(Reg::physical(RAX), GPR))
                .operand(Operand::read(Reg::physical(RCX), GPR))
                .finish();
        }

        // And which instruction each row is for, which the line table has no use for and the
        // locations do, since a stretch a local is somewhere over is named by an instruction at
        // each end and this is where one gets an address.
        let line: Vec<Inst> = func.blocks().flat_map(|block| func.insts(block)).collect();
        let out = assemble(&[func], &names, &target(), true, true).expect("two instructions");
        assert_eq!(
            out.lines,
            vec![vec![
                Row { at: 0, span: Span::new(0, 3), inst: Some(line[0]) },
                Row { at: 2, span: Span::new(10, 13), inst: Some(line[1]) },
            ]]
        );
    }

    #[test]
    fn a_function_that_knows_where_it_was_declared_says_so_over_its_prologue() {
        // The front of a function is instructions no expression in the source asked for, so
        // nothing there carries a span and the bytes would be covered by nothing. The declaration
        // is what gcc puts over them and it is what this puts over them too, as a row at zero in
        // front of everything the body produced.
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        func.declared = Span::new(100, 104);
        let block = func.create_block();
        let add = Opcode::new(names.intern("x64.add_rr_32"));
        // The first with no span, the way every instruction a prologue is made of has none, and
        // the second with one, the way an instruction the body asked for does.
        for span in [Span::DUMMY, Span::new(10, 13)] {
            func.build(block, add)
                .at(span)
                .operand(Operand::write(Reg::physical(RAX), GPR))
                .operand(Operand::read(Reg::physical(RAX), GPR))
                .operand(Operand::read(Reg::physical(RCX), GPR))
                .finish();
        }

        // The row for the declaration is the one row here no instruction wrote, which is what
        // says the bytes it covers are the prologue's.
        let line: Vec<Inst> = func.blocks().flat_map(|block| func.insts(block)).collect();
        let out = assemble(&[func], &names, &target(), true, true).expect("two instructions");
        assert_eq!(
            out.lines,
            vec![vec![
                Row { at: 0, span: Span::new(100, 104), inst: None },
                Row { at: 0, span: Span::DUMMY, inst: Some(line[0]) },
                Row { at: 2, span: Span::new(10, 13), inst: Some(line[1]) },
            ]]
        );
    }

    #[test]
    fn a_build_that_asked_for_none_carries_no_rows_at_all() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        add(&mut func, &mut names);

        let out = assemble(&[func], &names, &target(), true, false).expect("one instruction");
        assert_eq!(out.lines, vec![Vec::new()]);
    }

    #[test]
    fn a_machine_with_no_encoder_here_is_said_so_rather_than_encoded_as_x86_64() {
        let names = Interner::new();
        let aarch64 = TargetInfo::new(Triple::new(Arch::Aarch64, Os::Linux, Env::Gnu));
        let error = assemble(&[], &names, &aarch64, true, false).expect_err("no encoder");
        assert!(matches!(error, Error::Machine { .. }), "{error:?}");
    }

    /// A loop that would cross a line starts on the next one, the gap is instructions that do
    /// nothing, and a loop that fits where it falls is left there.
    #[test]
    fn the_head_of_a_loop_that_would_cross_a_line_starts_on_the_next_one() {
        let laid = |ahead: usize| {
            write(|func, names| {
                let first = func.create_block();
                let head = func.create_block();
                let add = Opcode::new(names.intern("x64.add_rr_32"));
                for block in std::iter::repeat_n(first, ahead).chain(std::iter::repeat_n(head, 15))
                {
                    func.build(block, add)
                        .operand(Operand::write(Reg::physical(RAX), GPR))
                        .operand(Operand::read(Reg::physical(RAX), GPR))
                        .operand(Operand::read(Reg::physical(RCX), GPR))
                        .finish();
                }
                func.build(head, Opcode::new(names.intern("x64.jmp"))).finish();
                func.succs_mut(head).push(BlockCall::to(head));
                func.heads = vec![head];
            })
        };
        // Fifteen adds and the five byte jump back are a loop of thirty five bytes. Twenty adds in
        // front put it at forty, which crosses at sixty four, so it moves there.
        let text = laid(20);
        assert_eq!(text.bytes.len(), 64 + 35);
        assert_eq!(hex(&text.bytes[64..66]), "01 c8");
        assert!(text.bytes[40..64].iter().all(|&byte| byte != 0x01), "only padding in the gap");
        assert_eq!(text.bytes[40], 0x66, "a long nop rather than single bytes");
        assert!(text.align >= 64, "{}", text.align);
        // Ten adds in front put it at twenty, and it ends at fifty five without crossing.
        let text = laid(10);
        assert_eq!(text.bytes.len(), 20 + 35);
        assert_eq!(hex(&text.bytes[20..22]), "01 c8");
    }
}
