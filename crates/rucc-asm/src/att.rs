//! Machine functions as assembly text, in AT&T syntax.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.1, which asks that the text path and the
//! binary path share one instruction description so they cannot disagree about what an
//! instruction is. This is the text path, and the description is `rucc_target::x86_64`.
//!
//! An AArch64 file is written by the same walk, since sections, labels, unwind rows and variables
//! are spelled the same way for both machines. Only the instruction is handed to [`crate::a64`].
//!
//! An i386 file is x86-64's, since the instructions are the same ones. What differs is written
//! where it comes up: an address is thirty two bits, so its registers are `%ecx` rather than
//! `%rcx`; there is nothing relative to the instruction pointer, so a symbol on its own is its
//! address; and the registers x86-64 added are refused rather than written.
//!
//! So there is almost nothing about x86-64 in this file. What an opcode is called, how many
//! instructions it really is, which operand each of them is given and how wide each of those is
//! written are all read out of the target. What is here is the syntax: a register carries a `%`,
//! an immediate carries a `$`, an address is a displacement in front of a parenthesised base and
//! index, and the source is written before the destination.
//!
//! Intel syntax, which section 11.1 requires as an input and which `-masm=intel` will ask for as
//! an output, is the other order, no sigils and a different spelling of an address. It is a
//! second walk over the same description rather than a second description, and it is not written
//! yet.
//!
//! # What a block is
//!
//! A label, and then the instructions in it. Where a block goes is on the block rather than on
//! its terminator, so a jump has already been made into an instruction by the block layout by
//! the time anything gets here: what is left is to give each block a name, and the name is local
//! so that it leaves no symbol behind for a debugger to show as if it were a function.
//!
//! # What the unwinder is told
//!
//! Every ELF function is wrapped in `.cfi_startproc` and `.cfi_endproc`, including the ones with
//! no rows in them. An unwinder that lands on an address with no record covering it has to give
//! up, so a leaf that never moves the stack pointer still needs a record: the one the CIE hands
//! it, which says the frame ends at `rsp+8` and the return address is the word below that, is
//! already the right answer for such a function and the empty record is how it asks for it.
//!
//! A COFF function is wrapped in `.seh_proc` and `.seh_endproc` instead, with the codes of its
//! prologue between, which is what gcc writes for Windows. They are worked out from the same rows
//! by the same code the object writer uses, so an object gas or the reader here assembles from the
//! listing has the same `.pdata` and `.xdata` as one written directly. Without them a function has
//! no row at all, and Windows takes it for a leaf and cannot unwind through it, which is what
//! a structured exception raised in `main` needs to do.
//!
//! A row written after the last instruction of the last block is dropped. It would describe an
//! address at or past the end of the function, which is outside what the record covers, and the
//! usual thing to find there is an epilogue putting back a state nothing is going to read.
//!
//! # What is not written
//!
//! An opcode that is not an instruction is written as nothing. Three of them exist to hold a
//! value in a register until something reads it, which is a fact the allocator needed and the
//! machine does not, and by here it has been acted on: the register in the operand is the answer.
//!
//! # What the assembler is told about extensions
//!
//! An AArch64 unit built for the CRC32 extension, by `-march=armv8-a+crc` or an architecture that
//! has it, opens with `.arch_extension crc`, and a function built for it by
//! `__attribute__((target("+crc")))` in a unit that is not has the directive before it and
//! `.arch_extension nocrc` after it, the other way round for a function that takes it away. The
//! `crc32` instructions come out of `<arm_acle.h>` as text, and an assembler not told about the
//! extension refuses them. gcc says the same with `.arch armv8-a+crc` at the top of the file and
//! around such a function. The extension is named rather than an architecture, because the CRC32
//! extension is all of `-march=` that is kept, and a directive naming the extension leaves
//! whatever else the assembler was told on its own command line alone.

use std::fmt::Write as _;

use rucc_base::Interner;
use rucc_base::hash::Map;
use rucc_mir::{Amode, Block, CfiOp, Flags, Func, Inst, Opcode, Operand, Reach, defs};
use rucc_object::{Alias, Binding, FUNC_ALIGN, Output, Sections};
use rucc_target::x86_64::{self, Arg, Width};
use rucc_target::{CallRegs, Feature, Isa, PhysReg, RegClass, TargetInfo, aarch64};
use rucc_tuple::Arch;

use crate::Error;
use crate::a64;
use crate::bytes::absolute_only;
use crate::data::{Globals, Piece, Variable};
use crate::format::{Directives, binding, visibility};
use crate::unwind::Prologue;

/// The prefix every x86-64 opcode carries in the machine IR.
///
/// An opcode is a name and a machine IR that holds two machines' instructions would otherwise
/// have two `add_rr_32` in it. The description in `rucc-target` is indexed without it, because
/// there it is already known which machine is being described.
const PREFIX: &str = "x64.";

/// Every function and every variable, as assembly text.
///
/// The functions first and the variables after them, which is the order every toolchain writes a
/// file in and the order a person reading one expects. The second names come last, because a
/// `.set` says nothing until the thing it names has been written down.
///
/// `unwind` is whether a function is described to an unwinder, which is
/// `rucc_session::Options::unwinds` and is asked of the build rather than worked out here, so that
/// this and the byte writer cannot answer it differently for one function.
///
/// `output` is whether each function and each variable is given a section of its own and what the
/// file says it was built to have checked, which are the other things about this listing the caller
/// decides: everything else is worked out from the functions and the target.
///
/// # Errors
///
/// [`Error::Machine`] for an architecture nothing here writes, and the two internal errors for a
/// function that should not have got this far. See [`Error`].
pub fn print(
    funcs: &[Func],
    globals: &Globals,
    aliases: &[Alias],
    names: &Interner,
    target: &TargetInfo,
    unwind: bool,
    output: Output,
) -> Result<String, Error> {
    listing(funcs, globals, aliases, names, target, unwind, output, false)
}

/// The same listing as [`print()`], with a label in front of every instruction named by [`mark`].
///
/// For an object built from a listing that asked for debug information. The listing has no line
/// table of its own, so the reader is left to place a label in front of each instruction the way
/// it places any other, and where those land is where the line table's rows go. The labels are
/// local ones, so the reader keeps them out of the symbol table like every other.
///
/// # Errors
///
/// As for [`print()`].
pub fn print_marked(
    funcs: &[Func],
    globals: &Globals,
    aliases: &[Alias],
    names: &Interner,
    target: &TargetInfo,
    unwind: bool,
    output: Output,
) -> Result<String, Error> {
    listing(funcs, globals, aliases, names, target, unwind, output, true)
}

/// The label [`print_marked`] writes in front of an instruction, given which function of the list
/// it is in and the instruction.
#[must_use]
pub fn mark(target: &TargetInfo, func: usize, inst: Inst) -> String {
    spell_mark(Directives::for_target(target), func, inst)
}

/// The label [`print_marked`] puts after the last instruction of the function with this place in
/// the list.
///
/// ELF says how long a function is with `.size` and the reader keeps it, but Mach-O has no such
/// directive, and how far this label is from the function's own symbol is the same length there.
#[must_use]
pub fn mark_end(target: &TargetInfo, func: usize) -> String {
    format!("{}rucc_end{func}", Directives::for_target(target).local())
}

fn spell_mark(directives: Directives, func: usize, inst: Inst) -> String {
    format!("{}rucc_row{func}_{}", directives.local(), inst.index())
}

#[allow(clippy::too_many_arguments)]
fn listing(
    funcs: &[Func],
    globals: &Globals,
    aliases: &[Alias],
    names: &Interner,
    target: &TargetInfo,
    unwind: bool,
    output: Output,
    marks: bool,
) -> Result<String, Error> {
    let Output { sections, property, isa, ident } = output;
    let arch = target.tuple.arch();
    if !matches!(arch, Arch::X86_64 | Arch::X86 | Arch::Aarch64) {
        return Err(Error::Machine { triple: target.tuple.to_string() });
    }
    let directives = Directives::for_target(target);
    let mut writer = Writer {
        arch,
        names,
        directives,
        // Apple's assembler reads the same directives gas does and so does the reader here, which
        // makes the DWARF table ld64 turns into its own. COFF's are other directives, the `.seh_`
        // ones, and they are asked for below instead.
        // Neither on i386 COFF, where Windows keeps no table of how to unwind a frame and gcc's
        // `.cfi_` directives would only be read and thrown away.
        unwind: unwind && !directives.coff(),
        seh: target.call_regs.filter(|_| unwind && directives == Directives::Coff),
        out: String::new(),
        labels: Vec::new(),
        sections,
        marks: marks.then_some(0),
        moved: false,
        crc: crc(isa),
    };
    if arch == Arch::Aarch64 && writer.crc {
        writer.out.push_str("\t.arch_extension\tcrc\n");
    }
    writer.out.push_str(writer.directives.text());
    writer.out.push('\n');
    // The `asm` at file scope that is instructions, first and between the markers gcc writes
    // around it, which the reader takes as comments. What section it leaves the assembler in is
    // its own business, so the functions after it are put back in the text section, which is
    // where they would have been without it.
    if !globals.file_asm.is_empty() {
        for text in &globals.file_asm {
            let _ = writeln!(writer.out, "#APP\n{text}\n#NO_APP");
        }
        writer.out.push_str(writer.directives.text());
        writer.out.push('\n');
    }
    // Each alias just after what it names, as gcc writes them, and one of an alias just after
    // that. That is the order the names go into the symbol table in, and the kernel's modpost
    // reads a file's device tables in that order. One whose target is not here goes last.
    let mut by_target: Map<&str, Vec<&Alias>> = Map::default();
    for alias in aliases {
        by_target.entry(alias.target.as_str()).or_default().push(alias);
    }
    for func in funcs {
        writer.func(func)?;
        writer.aliases(&mut by_target, names.resolve(func.name));
    }
    for var in &globals.vars {
        writer.variable(var);
        writer.aliases(&mut by_target, &var.name);
    }
    for alias in aliases {
        if by_target.contains_key(alias.target.as_str()) {
            writer.directives.alias(&mut writer.out, alias);
        }
    }
    // Last, which is where gcc puts them. Each is a name and no bytes, so there is nothing to open
    // a section for and nothing to close.
    for name in &globals.weak {
        writer.directives.absent(&mut writer.out, name);
    }
    for (name, visibility) in &globals.unseen {
        writer.directives.seen(&mut writer.out, name, Binding::Global, *visibility);
    }
    // What `dllexport` asks for, as the options the linker reads out of `.drectve`, one `.ascii` a
    // name the way clang writes them. Only COFF has anything in the list. See
    // [`rucc_object::Export`].
    if !globals.exports.is_empty() {
        writer.out.push_str("\t.section\t.drectve,\"yni\"\n");
        for export in &globals.exports {
            // An option from `#pragma comment` may have a quote in it, round a library name
            // with a space in it, and a backslash in a path.
            let option = export.option().replace('\\', "\\\\").replace('"', "\\\"");
            let _ = writeln!(writer.out, "\t.ascii\t\"{option}\"");
        }
    }
    // Padded to the width of an address, which is four bytes on i386 and eight everywhere else.
    let align = if writer.arch == Arch::X86 { 4 } else { 8 };
    writer.directives.end(&mut writer.out, property, align, ident);
    Ok(writer.out)
}

/// One template kept as text, filled in the way [`print()`] writes it into a listing.
///
/// For the byte writer, which reads a template on its own when the text allows it rather than
/// sending the whole unit to the assembler. See `crate::bytes::template`. What goes into the holes
/// is the same text either way, so a template cannot come out as one instruction in the listing and
/// another in the object.
pub(crate) fn template(
    func: &Func,
    block: Block,
    inst: Inst,
    names: &Interner,
    directives: Directives,
) -> Result<String, Error> {
    let mut writer = Writer {
        arch: Arch::X86_64,
        names,
        directives,
        unwind: false,
        seh: None,
        out: String::new(),
        labels: Vec::new(),
        sections: Sections::default(),
        marks: None,
        moved: false,
        crc: false,
    };
    writer.inst(func, block, inst, names.resolve(func.name))?;
    Ok(writer.out)
}

/// A file being written out.
struct Writer<'a> {
    /// The machine the instructions are for, which is the one thing about a file that decides how
    /// an instruction is spelled. See [`crate::a64`].
    arch: Arch,
    names: &'a Interner,
    directives: Directives,
    /// Whether each function is wrapped in an unwind record.
    unwind: bool,
    /// The calling convention the prologues were built against, on COFF when there is to be an
    /// unwind table, which is when each function is described with `.seh_` directives instead.
    seh: Option<&'static CallRegs>,
    out: String,
    /// The number each block is written as, indexed by its own, which is its place in the layout
    /// rather than the order somebody happened to create the blocks in.
    labels: Vec<u32>,
    /// Whether each function and each variable is given a section of its own.
    sections: Sections,
    /// Which function of the list is being written, in a listing that puts a label in front of
    /// every instruction, and `None` in one that does not. See [`print_marked`].
    marks: Option<usize>,
    /// Whether the function written last was in a section the program named, which leaves the
    /// assembler somewhere the next function must not follow it into.
    moved: bool,
    /// Whether the unit is built for AArch64's CRC32 extension, which is what the assembler was
    /// told at the top of the file. See the module documentation.
    crc: bool,
}

/// What the frame is at one place in a function, as the unwind rows say it: the register the
/// frame is measured from, how far above it the frame ends, and where each saved register is.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Frame {
    cfa: (u16, i32),
    saved: std::collections::BTreeMap<u16, i32>,
    /// Whether the return address is signed, which only AArch64 says anything about.
    signed: bool,
}

impl Frame {
    /// Where every x86-64 record starts, which is what the header the unwind writer gives every
    /// record says: the frame ends a word above the stack pointer, and the return address is the
    /// word below the end. See `crate::unwind`.
    fn called() -> Self {
        Self { cfa: (7, 8), saved: std::iter::once((16, -8)).collect(), signed: false }
    }

    /// The state after one row.
    fn apply(&mut self, op: CfiOp, stack: &mut Vec<Self>) {
        match op {
            CfiOp::DefCfa { reg, offset } => self.cfa = (reg, offset),
            CfiOp::DefCfaOffset(offset) => self.cfa.1 = offset,
            CfiOp::DefCfaRegister(reg) => self.cfa.0 = reg,
            CfiOp::Offset { reg, offset } => {
                self.saved.insert(reg, offset);
            }
            CfiOp::Restore(reg) => match Self::called().saved.get(&reg) {
                Some(&offset) => {
                    self.saved.insert(reg, offset);
                }
                None => {
                    self.saved.remove(&reg);
                }
            },
            CfiOp::RememberState => stack.push(self.clone()),
            CfiOp::RestoreState => {
                if let Some(state) = stack.pop() {
                    *self = state;
                }
            }
            CfiOp::NegateRaState => self.signed = !self.signed,
        }
    }

    /// The rows that take this state to that one.
    fn towards(&self, to: &Self, rows: &mut Vec<CfiOp>) {
        match (self.cfa.0 == to.cfa.0, self.cfa.1 == to.cfa.1) {
            (true, true) => {}
            (true, false) => rows.push(CfiOp::DefCfaOffset(to.cfa.1)),
            (false, true) => rows.push(CfiOp::DefCfaRegister(to.cfa.0)),
            (false, false) => rows.push(CfiOp::DefCfa { reg: to.cfa.0, offset: to.cfa.1 }),
        }
        for (&reg, &offset) in &to.saved {
            if self.saved.get(&reg) != Some(&offset) {
                rows.push(CfiOp::Offset { reg, offset });
            }
        }
        for &reg in self.saved.keys() {
            if !to.saved.contains_key(&reg) {
                rows.push(CfiOp::Restore(reg));
            }
        }
        if self.signed != to.signed {
            rows.push(CfiOp::NegateRaState);
        }
    }
}

/// The rows the cold part of a split function opens with, which take a fresh record to the state
/// the first part ended in, each remembered state on the way. See [`Writer::cold_part`].
pub(crate) fn cold_rows(func: &Func, cold: Block) -> Vec<CfiOp> {
    let mut now = Frame::called();
    let mut stack = Vec::new();
    for block in func.blocks().take_while(|&block| block != cold) {
        for inst in func.insts(block) {
            for op in func.cfi_after(inst) {
                now.apply(op, &mut stack);
            }
        }
    }
    let mut rows = Vec::new();
    let mut said = Frame::called();
    for state in &stack {
        said.towards(state, &mut rows);
        rows.push(CfiOp::RememberState);
        said = state.clone();
    }
    said.towards(&now, &mut rows);
    rows
}

/// Whether a set of extensions has AArch64's CRC32 one.
fn crc(isa: Isa) -> bool {
    Feature::aarch64("crc").is_some_and(|crc| isa.has(crc))
}

impl Writer<'_> {
    /// The directive that goes back to the section the program put this function in, and
    /// nothing for a function it said nothing about.
    ///
    /// A function written `retain` has a section of its own whether or not it named one, since
    /// the flag that keeps it is on the section and every other function in a shared one would
    /// be kept with it. It is `.text.` and the function's name, which is what gcc 16 calls it.
    /// Only ELF has the flag, and gcc refuses the attribute everywhere else.
    fn home(&self, func: &Func) -> Option<String> {
        let retain = func.retain && self.directives == Directives::Elf;
        let section = match func.section {
            Some(section) => self.names.resolve(section).to_owned(),
            None if retain => format!(".text.{}", self.names.resolve(func.name)),
            None => return None,
        };
        let mut directive = self.directives.named_code(&section);
        if retain {
            directive = directive.replacen("\"ax\"", "\"axR\"", 1);
        }
        Some(directive)
    }

    /// One function: what the assembler is told about it, then its blocks.
    fn func(&mut self, func: &Func) -> Result<(), Error> {
        let name = self.names.resolve(func.name).to_owned();
        self.number(func);
        let binding = binding(func.binding);
        let seen = visibility(func.visibility);
        let align = func.align.unwrap_or(FUNC_ALIGN);
        // A function built for other extensions than the unit is written between directives that
        // say so, which is what lets a `crc32` in it through the assembler.
        let wants = func.target.map_or(self.crc, crc);
        let switched = self.arch == Arch::Aarch64 && wants != self.crc;
        if switched {
            self.extension(wants);
        }
        // A function the program put in a section of its own goes there, and the one after it
        // goes back to wherever it would have gone, which is `.text` unless it has a section of
        // its own too. See [`Self::home`].
        let home = self.home(func);
        match &home {
            Some(section) => {
                let _ = writeln!(self.out, "{section}");
            }
            None if self.moved && !self.sections.functions => {
                let _ = writeln!(self.out, "{}", self.directives.text());
            }
            None => self.directives.code(&mut self.out, &name, self.sections),
        }
        self.moved = home.is_some();
        // What has to be written between what the assembler is told about the function and the
        // function's own label, which is nothing at all unless a patcher was promised room in
        // front of the label. See `patch`.
        let patch =
            func.patch.map(|patch| (patch, format!("{}pfe_{name}", self.directives.local())));
        let mut ahead = String::new();
        // The room a hot patcher was promised in front of the label, in the words gcc writes it
        // in. See `crate::hook`.
        if func.hook {
            for _ in 0..crate::hook::room(self.arch) / 4 {
                ahead.push_str("\t.long\t 0xcccccccc\n");
            }
        }
        if let Some((patch, label)) = &patch {
            let back = if let Some(section) = &home {
                section.clone()
            } else if self.sections.functions {
                format!("\t.section\t.text.{name}")
            } else {
                self.directives.text().to_owned()
            };
            self.directives.patchable(&mut ahead, label, &back);
            if patch.before > 0 {
                let _ = writeln!(ahead, "{label}:");
                self.pad(&mut ahead, patch.pad, patch.before);
            }
        }
        let fill = self.fill();
        self.directives.open(&mut self.out, &name, align, fill, binding, seen, &ahead);
        // And what it writes over, which is after the label and in front of everything the
        // unwinder is told, as gcc has it.
        let opening = crate::hook::opening(self.arch);
        if func.hook && !opening.is_empty() {
            self.out.push_str(&crate::hook::spelled(opening));
        }
        let unwind = self.unwind;
        if unwind {
            let _ = writeln!(self.out, "\t.cfi_startproc");
        }
        // An ARM64 function is described from its instructions rather than from its rows, so its
        // instructions are written out first and what goes around them worked out from that. See
        // `crate::arm_unwind`.
        let arm = match self.seh {
            Some(_) if self.arch == Arch::Aarch64 => Some(self.arm_unwind(func, &name)?),
            _ => None,
        };
        let seh = if arm.is_some() { None } else { self.prologue(func, &name)? };
        if seh.is_some() || arm.is_some() {
            let _ = writeln!(self.out, "\t.seh_proc\t{name}");
        }
        if seh.as_ref().is_some_and(|seh| seh.codes.is_empty())
            || arm.as_ref().is_some_and(|(_, plan)| plan.empty)
        {
            let _ = writeln!(self.out, "\t.seh_endprologue");
        }
        let mut line = 0;
        let mut place = 0;
        // The personality routine and the call site table, for a function with a landing pad, in
        // gcc's spelling, which is what the unwind writer puts in the object as well. See
        // `crate::unwind`. The calls with a pad are named on either side below, since the table
        // says where they are by label.
        //
        // Only on ELF, since both are spelled in ELF's terms: the routine through a slot gcc's
        // runtime fills in and the table in ELF's section for it. A Mach-O function is unwound
        // through without either. See `crate::unwind::table`.
        let pads: Map<Inst, Block> = if self.directives == Directives::Elf {
            func.landings.iter().copied().collect()
        } else {
            Map::default()
        };
        let local = self.directives.local();
        if unwind && !pads.is_empty() {
            let _ = writeln!(self.out, "\t.cfi_personality 0x9b,{}", rucc_ir::PERSONALITY_REF);
            let _ = writeln!(self.out, "\t.cfi_lsda 0x1b,{local}LSDA_{name}");
        }
        let mut sites = Vec::new();
        let end = func.cfi_end();
        // How long each loop is, which the listing cannot say without the lengths of the
        // instructions in it. The object writer's encoder is asked, and a function it cannot
        // encode, which is one for another machine, has its loops left where they fall.
        let loops = crate::bytes::loop_sizes(
            self.names,
            self.directives,
            func,
            &mut crate::bytes::Known::default(),
        )
        .unwrap_or_default();
        // A function split in two has its first part end at the last instruction in front of the
        // cold part, and the rows after that one are said at the top of the cold part instead.
        let split = func.cold.filter(|_| self.directives == Directives::Elf);
        let hot_end = split.and_then(|cold| {
            func.blocks()
                .take_while(|&block| block != cold)
                .flat_map(|block| func.insts(block))
                .last()
        });
        for (index, block) in func.blocks().enumerate() {
            if Some(block) == split {
                self.cold_part(func, block, &name);
            }
            let size = loops.get(block.index()).copied().unwrap_or(0);
            if let Some(most) = crate::loop_room(size) {
                let _ = writeln!(self.out, "\t.p2align\t6,,{most}");
            }
            let _ = writeln!(self.out, "{}{name}_{index}:", self.directives.local());
            // And the name an image knows the block by, as a second label on the same address. The
            // block's own label is written by this file and is a number, which is no good to a
            // relocation in another section: what that names is a symbol, and the name here is the
            // one the front end minted for it when it lowered the image. Spelled with the prefix
            // every other name in the listing gets, because the image and any instruction that
            // takes the address write it that way, and on Mach-O a label without the underscore
            // is a different name, which left `_.Llbl.0` undefined in every computed goto table.
            if let Some(label) = func.block_name(block) {
                let prefix = self.directives.symbol();
                let _ = writeln!(self.out, "{prefix}{}:", self.names.resolve(label));
            }
            for inst in func.insts(block) {
                // The other half of the room, which is named here rather than laid down here: the
                // instructions it is made of are in the entry block like any others, and all that
                // is missing is somewhere for the record to point. Named at the instruction rather
                // than at the top of the block because a landing pad goes in front of it, and the
                // room a patcher writes over does not include the pad.
                if let Some((patch, label)) = &patch {
                    if patch.before == 0 && patch.after == Some(inst) {
                        let _ = writeln!(self.out, "{label}:");
                    }
                }
                if let Some(which) = self.marks {
                    let _ = writeln!(self.out, "{}:", spell_mark(self.directives, which, inst));
                }
                let site = pads.get(&inst).map(|&pad| (sites.len(), pad));
                if let Some((at, _)) = site {
                    let _ = writeln!(self.out, "{local}EHB{at}_{name}:");
                }
                match &arm {
                    Some((texts, plan)) => {
                        for text in texts[place].split_inclusive('\n') {
                            if !crate::arm_unwind::machine(text) {
                                self.out.push_str(text);
                                continue;
                            }
                            for directive in &plan.before[line] {
                                let _ = writeln!(self.out, "{directive}");
                            }
                            self.out.push_str(text);
                            for directive in &plan.after[line] {
                                let _ = writeln!(self.out, "{directive}");
                            }
                            line += 1;
                        }
                    }
                    None => match func.mcount.filter(|mcount| mcount.inst == inst) {
                        Some(mcount) => self.mcount(func, block, mcount, &name)?,
                        None => self.inst(func, block, inst, &name)?,
                    },
                }
                if let Some((at, pad)) = site {
                    let _ = writeln!(self.out, "{local}EHE{at}_{name}:");
                    sites.push(pad);
                }
                if unwind && Some(inst) != end && Some(inst) != hot_end {
                    for op in func.cfi_after(inst) {
                        self.cfi(op);
                    }
                }
                if let Some(seh) = &seh {
                    for (_, code) in seh.codes.iter().filter(|(at, _)| *at == place) {
                        let _ = writeln!(self.out, "{code}");
                    }
                    if seh.codes.last().is_some_and(|(at, _)| *at == place) {
                        let _ = writeln!(self.out, "\t.seh_endprologue");
                    }
                }
                place += 1;
            }
        }
        if let Some(which) = self.marks {
            let _ = writeln!(self.out, "{}rucc_end{which}:", self.directives.local());
        }
        if split.is_some() {
            // The cold part ends the function, and what comes after it goes back to where the
            // first part is, which is what the next function expects to find itself in unless it
            // says otherwise. A split function is never one the program placed.
            if unwind {
                let _ = writeln!(self.out, "\t.cfi_endproc");
            }
            self.directives.close(&mut self.out, &format!("{name}.cold"));
            if self.sections.functions {
                self.directives.code(&mut self.out, &name, self.sections);
            } else {
                let _ = writeln!(self.out, "{}", self.directives.text());
            }
            self.tables(func, &name);
            if switched {
                self.extension(self.crc);
            }
            if let Some(which) = &mut self.marks {
                *which += 1;
            }
            return Ok(());
        }
        self.tables(func, &name);
        if seh.is_some() || arm.is_some() {
            let _ = writeln!(self.out, "\t.seh_endproc");
        }
        if unwind {
            let _ = writeln!(self.out, "\t.cfi_endproc");
            if !sites.is_empty() {
                self.call_sites(func, &name, &sites);
            }
        }
        self.directives.close(&mut self.out, &name);
        if switched {
            self.extension(self.crc);
        }
        if let Some(which) = &mut self.marks {
            *which += 1;
        }
        Ok(())
    }

    /// Ends the first part of a function split in two and starts the second, which is the
    /// function's name with `.cold` after it in `.text.unlikely`, the way gcc writes one.
    ///
    /// The second part is a function of its own as far as the unwind tables go, so it opens a
    /// record of its own, and that record starts where every record starts, which is the state a
    /// call leaves behind. The rows here take it from there to the state the first part was in
    /// where it ended, the remembered states included, so that a `.cfi_restore_state` in the cold
    /// part finds what it would have found in the first part.
    fn cold_part(&mut self, func: &Func, cold: Block, name: &str) {
        if self.unwind {
            let _ = writeln!(self.out, "\t.cfi_endproc");
        }
        self.directives.close(&mut self.out, name);
        if self.sections.functions {
            let _ = writeln!(self.out, "\t.section\t.text.unlikely.{name},\"ax\",@progbits");
        } else {
            let _ = writeln!(self.out, "\t.section\t.text.unlikely,\"ax\",@progbits");
        }
        if self.unwind {
            let _ = writeln!(self.out, "\t.cfi_startproc");
            for op in cold_rows(func, cold) {
                self.cfi(op);
            }
        }
        let _ = writeln!(self.out, "\t.type\t{name}.cold, @function");
        let _ = writeln!(self.out, "{name}.cold:");
    }

    /// Tells the assembler that the CRC32 extension is on from here, or off.
    fn extension(&mut self, on: bool) {
        let name = if on { "crc" } else { "nocrc" };
        let _ = writeln!(self.out, "\t.arch_extension\t{name}");
    }

    /// The codes a COFF function's prologue is described with, keyed by each instruction's place in
    /// the function rather than by a byte, or nothing when there is no table to write.
    ///
    /// The same rows the object writer reads and the same code turning them into codes, so a
    /// listing asks the assembler for exactly the record the object writer would have written. Each
    /// code is printed after the instruction it is about, which is where gas measures it from, and
    /// the end of the prologue after the last of them, which is where the object writer measures
    /// that from. A prologue the table cannot describe is refused here as it is there.
    fn prologue(&self, func: &Func, name: &str) -> Result<Option<Prologue>, Error> {
        let Some(conv) = self.seh else {
            return Ok(None);
        };
        let end = func.cfi_end();
        let mut rows = Vec::new();
        let insts = func.blocks().flat_map(|block| func.insts(block));
        for (place, inst) in insts.enumerate() {
            if Some(inst) != end {
                rows.extend(func.cfi_after(inst).map(|op| (place, op)));
            }
        }
        let codes = crate::unwind::prologue(name, &rows, conv)?;
        Ok(Some(Prologue { end: codes.last().map_or(0, |(at, _)| *at), codes }))
    }

    /// Every instruction of an ARM64 function as the listing writes it, by its place in the
    /// function, and the `.seh_` directives that go around them.
    ///
    /// The prologue ends at the instruction the row keeping the body's rules is written behind,
    /// which is where the frame code says the body starts. See `crate::arm_unwind`.
    fn arm_unwind(
        &self,
        func: &Func,
        name: &str,
    ) -> Result<(Vec<String>, crate::arm_unwind::Plan), Error> {
        let mut texts = Vec::new();
        let mut lines = Vec::new();
        let mut end = None;
        for (index, block) in func.blocks().enumerate() {
            for inst in func.insts(block) {
                let place = texts.len();
                let text = self.a64_text(func, block, inst, name)?;
                for line in
                    text.split_inclusive('\n').filter(|line| crate::arm_unwind::machine(line))
                {
                    let text = line.trim().to_owned();
                    lines.push(crate::arm_unwind::Line { text, block: index, place });
                }
                if end.is_none() && func.cfi_after(inst).any(|op| op == CfiOp::RememberState) {
                    end = Some(place);
                }
                texts.push(text);
            }
        }
        let plan = crate::arm_unwind::plan(name, &lines, end)?;
        Ok((texts, plan))
    }

    /// The instructions that do nothing which go in front of a function's own label.
    ///
    /// Written from the opcode rather than through the machinery every other instruction goes
    /// through, because these are the only instructions in a finished function that are not in a
    /// block and so are not instructions the function holds. The opcode is one with no operands,
    /// which is what makes writing the mnemonic and nothing else the whole of it.
    fn pad(&self, out: &mut String, pad: Opcode, count: u32) {
        let spelled = self.names.resolve(pad.name());
        let opcode = spelled.strip_prefix(self.prefix()).unwrap_or(spelled);
        for _ in 0..count {
            let _ = writeln!(out, "\t{opcode}");
        }
    }

    /// One row of the unwind table, as the directive an assembler reads it as.
    ///
    /// The registers are written as numbers rather than as names, which is what gcc writes and
    /// what avoids a second spelling of a register that could disagree with the first. They are
    /// DWARF's numbers, which are not the machine's, and the one place the mapping lives is the
    /// calling convention the prologue read it out of.
    fn cfi(&mut self, op: CfiOp) {
        let _ = match op {
            CfiOp::DefCfa { reg, offset } => {
                writeln!(self.out, "\t.cfi_def_cfa {reg}, {offset}")
            }
            CfiOp::DefCfaOffset(offset) => writeln!(self.out, "\t.cfi_def_cfa_offset {offset}"),
            CfiOp::DefCfaRegister(reg) => writeln!(self.out, "\t.cfi_def_cfa_register {reg}"),
            CfiOp::Offset { reg, offset } => writeln!(self.out, "\t.cfi_offset {reg}, {offset}"),
            CfiOp::Restore(reg) => writeln!(self.out, "\t.cfi_restore {reg}"),
            CfiOp::RememberState => writeln!(self.out, "\t.cfi_remember_state"),
            CfiOp::RestoreState => writeln!(self.out, "\t.cfi_restore_state"),
            CfiOp::NegateRaState => writeln!(self.out, "\t.cfi_negate_ra_state"),
        };
    }

    /// The aliases of `target` that are still to be written, and then those of each of them.
    fn aliases(&mut self, by_target: &mut Map<&str, Vec<&Alias>>, target: &str) {
        let Some(named) = by_target.remove(target) else { return };
        for alias in named {
            self.directives.alias(&mut self.out, alias);
            self.aliases(by_target, &alias.name);
        }
    }

    /// One variable: what the assembler is told about it, then its image.
    fn variable(&mut self, var: &Variable) {
        if !self.directives.variable(&mut self.out, var, self.sections) {
            return;
        }
        for piece in &var.pieces {
            self.piece(piece);
        }
        self.directives.close(&mut self.out, &var.name);
        self.directives.descriptor(&mut self.out, var);
    }

    /// One piece of an image, as the directive that says it.
    ///
    /// A number is written at the width it is rather than as the bytes it is made of, because the
    /// point of a listing is to be read and `.long 258` is what a person wrote. The bytes are the
    /// same either way, which is what [`crate::data`] is for.
    fn piece(&mut self, piece: &Piece) {
        match piece {
            // `.space` rather than `.zero`, which every assembler also takes, because the
            // directive set `spec/11-asm-objects-debug.md` says this compiler's own assembler
            // reads has the one and not the other in it.
            Piece::Zero(bytes) => {
                let _ = writeln!(self.out, "\t.space\t{bytes}");
            }
            Piece::Bytes(bytes) => {
                let _ = writeln!(self.out, "\t.ascii\t\"{}\"", escape(bytes));
            }
            Piece::Scalar(bytes) => match width(bytes.len()) {
                Some(directive) => {
                    let mut value = [0u8; 16];
                    value[..bytes.len()].copy_from_slice(bytes);
                    let _ = writeln!(self.out, "\t{directive}\t{}", u128::from_le_bytes(value));
                }
                // A width no directive names, which on this machine is the eighty bit float and
                // nothing else. Its bytes are what it is.
                None => {
                    let list = bytes.iter().map(u8::to_string).collect::<Vec<_>>().join(", ");
                    let _ = writeln!(self.out, "\t.byte\t{list}");
                }
            },
            // Four and eight are the only widths that reach here, because the walk that built
            // this refused every other one rather than leave the two halves to disagree.
            // Written the way the template that asked for it wrote it, which is the one spelling
            // an assembler reading this back would give the same relocation to.
            Piece::Away { symbol, addend } => {
                let name = self.directives.spell(symbol);
                match addend {
                    0 => {
                        let _ = writeln!(self.out, "\t.long\t{name} - .");
                    }
                    _ => {
                        let sign = if *addend < 0 { '-' } else { '+' };
                        let _ = writeln!(self.out, "\t.long\t{name}{sign}{} - .", addend.abs());
                    }
                }
            }
            Piece::Addr { symbol, addend, bytes } => {
                let directive = if *bytes == 8 { ".quad" } else { ".long" };
                let name = self.directives.spell(symbol);
                match addend {
                    0 => {
                        let _ = writeln!(self.out, "\t{directive}\t{name}");
                    }
                    _ => {
                        let sign = if *addend < 0 { '-' } else { '+' };
                        let _ = writeln!(self.out, "\t{directive}\t{name}{sign}{}", addend.abs());
                    }
                }
            }
            // Spelled the way a jump table's cells are, which is what gas reads as a number it
            // works out itself when both labels are in one section.
            Piece::Apart { to, from, addend, bytes } => {
                let directive = width(usize::from(*bytes)).unwrap_or(".long");
                let (to, from) = (self.directives.spell(to), self.directives.spell(from));
                let _ = write!(self.out, "\t{directive}\t{to}-{from}");
                let _ = match addend.signum() {
                    1 => writeln!(self.out, "+{addend}"),
                    -1 => writeln!(self.out, "-{}", addend.unsigned_abs()),
                    _ => writeln!(self.out),
                };
            }
        }
    }

    /// Gives every block the number its label carries.
    fn number(&mut self, func: &Func) {
        self.labels.clear();
        self.labels.resize(func.block_count(), u32::MAX);
        for (index, block) in func.blocks().enumerate() {
            self.labels[block.index()] = u32::try_from(index).expect("a block number");
        }
    }

    /// One ARM64 instruction as the listing writes it, which can be more than one line.
    fn a64_text(
        &self,
        func: &Func,
        block: Block,
        inst: Inst,
        func_name: &str,
    ) -> Result<String, Error> {
        let spelling = match self.directives {
            Directives::MachO => aarch64::Spelling::Apple,
            Directives::Elf | Directives::Coff | Directives::CoffI386 => aarch64::Spelling::Gnu,
        };
        let at = a64::Context {
            names: self.names,
            symbol: self.directives.symbol(),
            spelling,
            func_name,
        };
        let mut line = String::new();
        let label = |to| self.label(func_name, to);
        let table = |at: u32| self.table(func_name, at as usize);
        a64::inst(&mut line, &at, func, block, inst, label, table)?;
        Ok(line)
    }

    /// The profiler's call, when `-mrecord-mcount` or `-mnop-mcount` asked for something to be done
    /// with it, in gcc's spelling: a label on the call, or on the nop in its place, and the label's
    /// address, a `.quad` or on i386 a `.long`, in `__mcount_loc` straight after it, or in the section `fentry_section` or
    /// `-mfentry-section=` named. See [`rucc_mir::Mcount`].
    fn mcount(
        &mut self,
        func: &Func,
        block: Block,
        mcount: rucc_mir::Mcount,
        name: &str,
    ) -> Result<(), Error> {
        let label = format!("{}mcount_{name}", self.directives.local());
        if mcount.record {
            let _ = writeln!(self.out, "{label}:");
        }
        if mcount.nop {
            let _ = writeln!(self.out, "\t.byte\t0x0f, 0x1f, 0x44, 0x00, 0x00");
        } else {
            self.inst(func, block, mcount.inst, name)?;
        }
        if mcount.record {
            let section = mcount
                .section
                .map_or(rucc_object::MCOUNT_LOC, |section| self.names.resolve(section));
            let _ = writeln!(self.out, "\t.section\t{section},\"a\",@progbits");
            let address = if self.arch == Arch::X86 { ".long" } else { ".quad" };
            let _ = writeln!(self.out, "\t{address}\t{label}");
            let _ = writeln!(self.out, "\t.previous");
        }
        Ok(())
    }

    /// One instruction of the machine IR, as however many instructions of the machine it is.
    fn inst(
        &mut self,
        func: &Func,
        block: Block,
        inst: Inst,
        func_name: &str,
    ) -> Result<(), Error> {
        if self.arch == Arch::Aarch64 {
            let line = self.a64_text(func, block, inst, func_name)?;
            self.out.push_str(&line);
            return Ok(());
        }
        let data = func[inst];
        let spelled = self.names.resolve(data.opcode.name());
        let opcode = spelled.strip_prefix(PREFIX).unwrap_or(spelled);
        // The one opcode that is not an instruction and is still written down. Everything below
        // spells a mnemonic and its arguments, and this has neither: what it says is where the next
        // instruction starts, which in a listing is the assembler's own directive. The fill byte is
        // the one that does nothing, because a gap in the middle of a function is reached by falling
        // into it rather than by jumping over it.
        if opcode == x86_64::ALIGN {
            let bytes = data.imm.map_or(0, |imm| func[imm].0);
            let boundary = u32::try_from(bytes).ok().filter(|at| at.is_power_of_two());
            let Some(boundary) = boundary else {
                return Err(Error::Opcode {
                    func: func_name.to_owned(),
                    opcode: spelled.to_owned(),
                });
            };
            let _ = writeln!(self.out, "\t.p2align\t{}, 0x90", boundary.trailing_zeros());
            return Ok(());
        }
        // The other one, which is the bytes a template wrote out as themselves. They come back the
        // way they went in, since the directive is what the program wrote and an assembler reading
        // this listing has to get the same bytes out of it. One directive, because that is how many
        // the instruction carries.
        if opcode == x86_64::LITERAL {
            let bytes: Vec<u8> =
                data.imm.map(|imm| x86_64::unpacked(func[imm].0).collect()).unwrap_or_default();
            if bytes.is_empty() {
                return Err(Error::Opcode {
                    func: func_name.to_owned(),
                    opcode: spelled.to_owned(),
                });
            }
            let written: Vec<String> = bytes.iter().map(|byte| format!("0x{byte:02x}")).collect();
            let _ = writeln!(self.out, "\t.byte\t{}", written.join(", "));
            return Ok(());
        }
        // A template kept as text, written back as the text with what it names filled in. Each of
        // its lines goes down as a line of the listing, which is where gcc puts one too.
        if opcode == x86_64::TEMPLATE {
            let Some(text) = data.symbol else {
                return Err(Error::Opcode {
                    func: func_name.to_owned(),
                    opcode: spelled.to_owned(),
                });
            };
            let mem = match data.mem {
                Some(mem) => self.amode(&func[data.operands], &func[mem], func_name, spelled)?,
                None => String::new(),
            };
            // A register it names is the operand's, spelled at the width the hole says. Every one
            // of them has a register by now, and one that has not is the same mistake it is in any
            // other instruction.
            let operands = &func[data.operands];
            if operands.iter().any(|operand| operand.reg.phys().is_none()) {
                return Err(Error::Virtual {
                    func: func_name.to_owned(),
                    opcode: spelled.to_owned(),
                });
            }
            // A label an `asm goto` jumps to is the block that arm of this one goes to, which has
            // a name once the layout has numbered the blocks. A template read on its own has no
            // numbers to go by, and one that jumps is then only right in the listing.
            let arms = &func[block].succs;
            let text = x86_64::template_arms(self.names.resolve(text), |arm| {
                let to = arms.get(arm)?.block;
                self.labels
                    .get(to.index())
                    .is_some_and(|&number| number != u32::MAX)
                    .then(|| self.label(func_name, to))
            })
            .ok_or_else(|| Error::Encode {
                func: func_name.to_owned(),
                opcode: spelled.to_owned(),
                why: "it jumps to a block, which only the whole listing can name".to_owned(),
            })?;
            let directives = self.directives;
            let word = self.word();
            let reg = |at: usize, width: char| {
                let Some(operand) = operands.get(at) else { return String::from("?") };
                let phys = operand.reg.phys().expect("every operand was checked above");
                // A capital is the same width with no `%`, which is what `%V` asks for.
                let bare = width.is_ascii_uppercase();
                let named = match width.to_ascii_lowercase() {
                    'b' => name_of(operand.class, phys, Width::Byte),
                    'w' => name_of(operand.class, phys, Width::Word),
                    'k' => name_of(operand.class, phys, Width::Long),
                    'h' => x86_64::gpr_high(phys).unwrap_or("?"),
                    _ => name_of(operand.class, phys, word),
                };
                if bare { named.to_owned() } else { format!("%{named}") }
            };
            let filled = x86_64::template_filled(&text, &mem, |name| directives.spell(name), reg);
            for line in filled.lines() {
                let _ = writeln!(self.out, "\t{}", line.trim_start());
            }
            return Ok(());
        }
        let Some(written) = x86_64::written(opcode) else {
            return Err(Error::Opcode { func: func_name.to_owned(), opcode: spelled.to_owned() });
        };
        let operands = &func[data.operands];
        for machine in written {
            let mut args = Vec::with_capacity(machine.args.len());
            for arg in machine.args {
                args.push(match *arg {
                    Arg::Reg(at, width) => {
                        let operand = operands[usize::from(at)];
                        self.reg(operand, width, func_name, spelled)?
                    }
                    // A whole vector register, whose name the register file holds outright. The
                    // width is asked for anyway because the one thing that reads it is the general
                    // purpose file, and a register in any other class has one name.
                    Arg::Xmm(at) => {
                        let operand = operands[usize::from(at)];
                        self.reg(operand, Width::Quad, func_name, spelled)?
                    }
                    // The two halves of one word, which is the one instruction that names part of
                    // a register rather than an amount of it. The low half is the byte the name
                    // above would give it and the high half is the one only four registers have,
                    // which is why the operand is fixed to one of the four where it is described.
                    Arg::Low(at) => {
                        let operand = operands[usize::from(at)];
                        self.reg(operand, Width::Byte, func_name, spelled)?
                    }
                    Arg::High(at) => {
                        let operand = operands[usize::from(at)];
                        let Some(phys) = operand.reg.phys() else {
                            return Err(Error::Virtual {
                                func: func_name.to_owned(),
                                opcode: spelled.to_owned(),
                            });
                        };
                        format!("%{}", x86_64::gpr_high(phys).unwrap_or("?"))
                    }
                    Arg::Named(register) => format!("%{register}"),
                    // A depth on the x87 stack rather than a register, which is why the number
                    // comes from the table and not from an operand. The assembler writes the top
                    // of the stack as `%st` on its own as well, and this writes `%st(0)` for it,
                    // because one spelling for all eight is one thing fewer to know.
                    Arg::Stack(depth) => format!("%st({depth})"),
                    Arg::Lit(lane) => format!("${lane}"),
                    // The first operand read, which is where a call puts the address it goes
                    // through. Everything in front of it is a register the call writes.
                    Arg::Through => {
                        let operand = operands[defs(operands)];
                        format!("*{}", self.reg(operand, self.word(), func_name, spelled)?)
                    }
                    Arg::Imm => match data.imm {
                        Some(imm) => format!("${}", func[imm].0),
                        None => "$0".to_owned(),
                    },
                    Arg::Mem => match data.mem {
                        Some(mem) => self.amode(operands, &func[mem], func_name, spelled)?,
                        None => "0".to_owned(),
                    },
                    // The address a call or a jump reads where it goes from, with the star that
                    // says so in front of it.
                    Arg::Indirect => match data.mem {
                        Some(mem) => {
                            format!("*{}", self.amode(operands, &func[mem], func_name, spelled)?)
                        }
                        None => "*0".to_owned(),
                    },
                    Arg::Symbol => match data.symbol {
                        // Through the procedure linkage table, which only the suffix says. See
                        // [`Flags::PLT`].
                        Some(symbol) if data.flags.contains(Flags::PLT) => {
                            format!("{}@PLT", self.directives.spell(self.names.resolve(symbol)))
                        }
                        Some(symbol) => self.directives.spell(self.names.resolve(symbol)),
                        None => "0".to_owned(),
                    },
                    // Where a conditional jump goes is the first arm, because the block layout
                    // guarantees the second is the block laid out next and is fallen into. An
                    // unconditional jump has one arm and it is the same one.
                    Arg::Label => match func[block].succs.first() {
                        Some(call) => self.label(func_name, call.block),
                        None => "0".to_owned(),
                    },
                });
            }
            // The address of a name under the kernel code model is the name as a number, and gcc
            // writes it as the immediate of a `movq` rather than as a `leaq` of an address with
            // no registers in it, which is a byte shorter and the same `R_X86_64_32S`.
            let mut mnemonic = machine.mnemonic;
            if mnemonic == "leaq" && data.mem.is_some_and(|mem| absolute_only(&func[mem])) {
                mnemonic = "movq";
                args[0] = format!("${}", args[0]);
            }
            if args.is_empty() {
                let _ = writeln!(self.out, "\t{mnemonic}");
            } else {
                let _ = writeln!(self.out, "\t{mnemonic}\t{}", args.join(", "));
            }
        }
        Ok(())
    }

    /// One register operand, as much of it as the instruction reads or writes.
    fn reg(
        &self,
        operand: Operand,
        width: Width,
        func_name: &str,
        opcode: &str,
    ) -> Result<String, Error> {
        let Some(phys) = operand.reg.phys() else {
            return Err(Error::Virtual { func: func_name.to_owned(), opcode: opcode.to_owned() });
        };
        // i386 has the first eight of each file and no general purpose one at sixty four bits. The description
        // is x86-64's and names both, so what keeps them out of an i386 listing is this, rather
        // than a name the assembler would read as something else or refuse.
        let missing = phys.number() >= 8 || (operand.class == x86_64::GPR && width == Width::Quad);
        if self.arch == Arch::X86 && operand.class != x86_64::X87 && missing {
            return Err(Error::Encode {
                func: func_name.to_owned(),
                opcode: opcode.to_owned(),
                why: format!("i386 has no register {}", name_of(operand.class, phys, width)),
            });
        }
        Ok(format!("%{}", name_of(operand.class, phys, width)))
    }

    /// How wide a register holding an address is, which is the width a base, an index and a
    /// register called through are written at.
    fn word(&self) -> Width {
        if self.arch == Arch::X86 { Width::Long } else { Width::Quad }
    }

    /// One address, which is a displacement and then whichever registers it names.
    ///
    /// A symbol with no base and no index is written relative to the instruction pointer, which
    /// is how a global is reached in position independent code and is the only way this compiler
    /// reaches one. A block is written the same way, and is the address a `&&label` produces.
    fn amode(
        &self,
        operands: &[Operand],
        amode: &Amode,
        func_name: &str,
        opcode: &str,
    ) -> Result<String, Error> {
        let mut out = String::new();
        // In front of everything, which is where an assembler wants it: the segment says which
        // storage the rest of the address is counted in, so `%fs:40` reads left to right.
        if let Some(segment) = amode.segment {
            let _ = write!(out, "%{}:", segment.name());
        }
        if let Some(symbol) = amode.symbol {
            let _ = write!(out, "{}", self.directives.spell(self.names.resolve(symbol)));
            // The slot rather than the thing, which the assembler is told by the suffix and not by
            // the instruction: the two are the same `movq` and differ only in what goes in the
            // four bytes, so there is nowhere else to say it. The third one is a slot as well, and
            // what it holds is an offset into a thread's own block rather than an address. On
            // Mach-O the slot holds the address of the variable's descriptor instead, which is
            // what the code calls through.
            // i386 has no addressing relative to the instruction pointer, so a slot is reached from
            // the table's own address in a register and the suffixes are the ones that count from
            // there. A thread's offset comes out of the table with nothing in front of it, or
            // from the table's address when there is one.
            let i386 = self.arch == Arch::X86;
            match amode.reach {
                Reach::Itself => {}
                Reach::Table if i386 => out.push_str("@GOT"),
                Reach::Thread if i386 && amode.base.is_some() => out.push_str("@GOTNTPOFF"),
                Reach::Thread if i386 => out.push_str("@INDNTPOFF"),
                Reach::Table => out.push_str("@GOTPCREL"),
                Reach::Thread if self.directives == Directives::MachO => out.push_str("@TLVP"),
                Reach::Thread => out.push_str("@GOTTPOFF"),
                // How far into its section, which is how far into a thread's copy of `.tls`.
                Reach::Section => out.push_str("@SECREL32"),
                // The address itself as a number, which needs no suffix and no `(%rip)`: gas
                // writes `R_X86_64_32S` for four bytes of it the machine sign extends.
                Reach::Absolute => {}
                // How far from the global offset table, whose address is in the base register.
                Reach::GotOff => out.push_str("@GOTOFF"),
            }
            if amode.disp != 0 {
                let sign = if amode.disp < 0 { '-' } else { '+' };
                let _ = write!(out, "{sign}{}", i64::from(amode.disp).abs());
            }
        } else if let Some(block) = amode.block {
            // A label of this function, which is written the way a symbol is and reached the way a
            // symbol is, and is neither: what the assembler puts in the four bytes is a distance it
            // works out itself, since both ends are in the section it is writing.
            out.push_str(&self.label(func_name, block));
            // i386 position independent code has no instruction pointer to count from, so the
            // label is counted from the global offset table in the base register instead.
            if amode.reach == Reach::GotOff {
                out.push_str("@GOTOFF");
            }
            if amode.disp != 0 {
                let sign = if amode.disp < 0 { '-' } else { '+' };
                let _ = write!(out, "{sign}{}", i64::from(amode.disp).abs());
            }
        } else if let Some(table) = amode.table {
            // A jump table of this function, which is a place in it the way a label is. Under the
            // kernel code model it is the table's own address instead, with an index beside it.
            out.push_str(&self.table(func_name, table as usize));
            if amode.reach == Reach::GotOff {
                out.push_str("@GOTOFF");
            }
            if amode.disp != 0 {
                let sign = if amode.disp < 0 { '-' } else { '+' };
                let _ = write!(out, "{sign}{}", i64::from(amode.disp).abs());
            }
        } else if amode.disp != 0 || (amode.base.is_none() && amode.index.is_none()) {
            // A mode that names no register at all is an absolute address, and zero is one of
            // them, so the number is written even when it is zero and there is nothing else.
            let _ = write!(out, "{}", amode.disp);
        }
        let base = amode.base.and_then(|at| operands.get(usize::from(at)));
        let index = amode.index.and_then(|at| operands.get(usize::from(at)));
        if base.is_some() || index.is_some() {
            out.push('(');
            if let Some(operand) = base {
                out.push_str(&self.reg(*operand, self.word(), func_name, opcode)?);
            }
            if let Some(operand) = index {
                let reg = self.reg(*operand, self.word(), func_name, opcode)?;
                let _ = write!(out, ",{reg},{}", amode.scale);
            }
            out.push(')');
        } else if self.arch == Arch::X86 {
            // Nothing: with no register in it, an address on i386 is the number itself.
        } else if (amode.symbol.is_some() && amode.reach != Reach::Absolute)
            || amode.block.is_some()
            || (amode.table.is_some() && amode.reach != Reach::Absolute)
        {
            out.push_str("(%rip)");
        }
        Ok(out)
    }

    /// The jump tables, where the encoder puts them. Each cell is the distance from the table to a
    /// block. See `bytes::Assembler::tables`.
    ///
    /// On x86-64 ELF they go in `.rodata`, or `.rodata.` and the function's name under
    /// `-fdata-sections`, which is what gcc writes, and the listing goes back to the function's
    /// own section afterwards so that what follows is still inside it. The assembler turns each
    /// cell into a relocation, since its two ends are in different sections. Everywhere else they
    /// stay after the last instruction, where the assembler works each cell out itself.
    ///
    /// A table the kernel code model rewrote holds each block's address in eight bytes instead,
    /// which is what gcc writes there and what objtool reads. See `rucc_codegen::fold::tables`.
    fn tables(&mut self, func: &Func, func_name: &str) {
        if func.tables.is_empty() {
            return;
        }
        let apart = self.arch == Arch::X86_64 && self.directives == Directives::Elf;
        if apart {
            let _ = if self.sections.data {
                writeln!(self.out, "\t.section\t.rodata.{func_name},\"a\",@progbits")
            } else {
                writeln!(self.out, "\t.section\t.rodata")
            };
            let wide = func.tables.iter().any(|table| table.absolute);
            let _ = writeln!(self.out, "\t.p2align\t{}", if wide { 3 } else { 2 });
        } else {
            let _ = match self.fill() {
                Some(byte) => writeln!(self.out, "\t.p2align\t2, {byte:#x}"),
                None => writeln!(self.out, "\t.p2align\t2"),
            };
        }
        for (index, table) in func.tables.iter().enumerate() {
            let label = self.table(func_name, index);
            let _ = writeln!(self.out, "{label}:");
            let block = func.block_of(table.jump).expect("a table read by a jump in no block");
            let succs = &func[block].succs;
            for &cell in &table.cells {
                let to = self.label(func_name, succs[cell as usize].block);
                let _ = if table.absolute {
                    writeln!(self.out, "\t.quad\t{to}")
                } else {
                    writeln!(self.out, "\t.long\t{to}-{label}")
                };
            }
        }
        if apart {
            if let Some(section) = self.home(func) {
                let _ = writeln!(self.out, "{section}");
            } else if self.sections.functions {
                let _ = writeln!(self.out, "\t.section\t.text.{func_name},\"ax\",@progbits");
            } else {
                let _ = writeln!(self.out, "{}", self.directives.text());
            }
        }
    }

    /// The call site table of a function with a landing pad, in its own section as gcc writes it.
    ///
    /// No landing pad base and no type table, then one row per call: where it starts and how long
    /// it is, where its pad is, and an action of zero, which says the pad is a cleanup. Every
    /// distance is from the function's own label, which is what a missing base means. `sites` is
    /// the pad of each call in the order the calls were written, which is the order the unwinder
    /// wants them in.
    fn call_sites(&mut self, func: &Func, name: &str, sites: &[Block]) {
        let local = self.directives.local();
        let _ = writeln!(self.out, "\t.section\t{},\"a\",@progbits", rucc_object::EXCEPT_TABLE);
        let _ = writeln!(self.out, "\t.p2align\t2");
        let _ = writeln!(self.out, "{local}LSDA_{name}:");
        let _ = writeln!(self.out, "\t.byte\t0xff\n\t.byte\t0xff\n\t.byte\t0x1");
        let _ = writeln!(self.out, "\t.uleb128\t{local}LSDACSE_{name}-{local}LSDACSB_{name}");
        let _ = writeln!(self.out, "{local}LSDACSB_{name}:");
        for (at, &pad) in sites.iter().enumerate() {
            let pad = self.label(name, pad);
            let _ = writeln!(self.out, "\t.uleb128\t{local}EHB{at}_{name}-{name}");
            let _ = writeln!(self.out, "\t.uleb128\t{local}EHE{at}_{name}-{local}EHB{at}_{name}");
            let _ = writeln!(self.out, "\t.uleb128\t{pad}-{name}");
            let _ = writeln!(self.out, "\t.uleb128\t0");
        }
        let _ = writeln!(self.out, "{local}LSDACSE_{name}:");
        // Back to the function's section, since what closes the function measures its size from
        // where the assembler is.
        if let Some(section) = self.home(func) {
            let _ = writeln!(self.out, "{section}");
        } else if self.sections.functions {
            let _ = writeln!(self.out, "\t.section\t.text.{name},\"ax\",@progbits");
        } else {
            let _ = writeln!(self.out, "{}", self.directives.text());
        }
    }

    /// The label one jump table of one function carries. The `j` is what keeps it apart from a
    /// block's label, which is a number after the same underscore.
    fn table(&self, func_name: &str, index: usize) -> String {
        format!("{}{func_name}_j{index}", self.directives.local())
    }

    /// What the machine IR puts in front of an opcode of this machine.
    fn prefix(&self) -> &'static str {
        if self.arch == Arch::Aarch64 { a64::PREFIX } else { PREFIX }
    }

    /// The byte padding inside code is made of, which is the one byte `nop` on x86-64. AArch64 has
    /// no one byte instruction, so the assembler is left to pad with its own `nop`.
    fn fill(&self) -> Option<u8> {
        if self.arch == Arch::Aarch64 { None } else { Some(0x90) }
    }

    /// The label one block of one function carries.
    fn label(&self, func_name: &str, block: Block) -> String {
        match self.labels.get(block.index()).copied() {
            Some(u32::MAX) | None => format!("{}{func_name}_?", self.directives.local()),
            Some(number) => format!("{}{func_name}_{number}", self.directives.local()),
        }
    }
}

/// The directive that writes a number that many bytes wide, and `None` for a width none does.
fn width(bytes: usize) -> Option<&'static str> {
    match bytes {
        1 => Some(".byte"),
        2 => Some(".short"),
        4 => Some(".long"),
        8 => Some(".quad"),
        _ => None,
    }
}

/// Those bytes as the inside of a string an assembler reads back as the same bytes.
///
/// Everything outside printable ASCII is written as three octal digits rather than as itself,
/// which is what keeps a string with a newline in it on one line and what stops a digit after an
/// escape from being read as part of it.
fn escape(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for byte in bytes {
        match byte {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            0x20..=0x7e => out.push(char::from(*byte)),
            _ => {
                let _ = write!(out, "\\{byte:03o}");
            }
        }
    }
    out
}

/// What one register is called, at that width, without the sigil.
///
/// The width is a general purpose register's business and nothing else's on this machine, since
/// every other class here has one name per register, which is the name the register file gives.
fn name_of(class: RegClass, reg: PhysReg, width: Width) -> &'static str {
    let named = if class == x86_64::GPR {
        x86_64::gpr_name(reg, width)
    } else {
        x86_64::REGS.name(class, reg)
    };
    named.unwrap_or("?")
}

#[cfg(test)]
mod tests {
    use super::*;

    use rucc_base::Interner;
    use rucc_mir::{Func, Mem, Operand, Reg};
    use rucc_object::{Binding, Place, Visibility};
    use rucc_target::x86_64::{GPR, RAX, RCX, RDX, RSP};
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    /// A target of that object format, which is what decides how a symbol is spelled.
    fn target(os: Os) -> TargetInfo {
        TargetInfo::new(Triple::new(Arch::X86_64, os, Env::Gnu))
    }

    /// One function of one block, with those instructions in it, written out.
    fn write(build: impl FnOnce(&mut Func, &mut Interner)) -> String {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        build(&mut func, &mut names);
        print(
            &[func],
            &Globals::default(),
            &[],
            &names,
            &target(Os::Linux),
            true,
            Output::default(),
        )
        .expect("a function that was allocated")
    }

    /// Those variables, written out for that object format.
    fn data(vars: Vec<Variable>, os: Os) -> String {
        let names = Interner::new();
        print(
            &[],
            &Globals { vars, ..Globals::default() },
            &[],
            &names,
            &target(os),
            true,
            Output::default(),
        )
        .expect("a machine with a writer")
    }

    /// The same, with every variable given a section of its own.
    fn split(vars: Vec<Variable>, os: Os) -> String {
        let names = Interner::new();
        let sections =
            Output { sections: Sections { functions: false, data: true }, ..Output::default() };
        print(
            &[],
            &Globals { vars, ..Globals::default() },
            &[],
            &names,
            &target(os),
            true,
            sections,
        )
        .expect("a machine with a writer")
    }

    /// Two functions of those names, written out with each of them given a section of its own.
    fn split_code(first: &str, second: &str, os: Os) -> String {
        let mut names = Interner::new();
        let mut funcs = Vec::new();
        for name in [first, second] {
            let mut func = Func::new(names.intern(name));
            func.create_block();
            funcs.push(func);
        }
        let sections =
            Output { sections: Sections { functions: true, data: false }, ..Output::default() };
        print(&funcs, &Globals::default(), &[], &names, &target(os), true, sections)
            .expect("a machine with a writer")
    }

    /// A four byte variable of that name, in that section, holding that image.
    fn var(name: &str, place: Place, pieces: Vec<Piece>) -> Variable {
        Variable {
            name: name.to_owned(),
            size: 4,
            align: 4,
            place,
            binding: Binding::Global,
            visibility: Visibility::Default,
            retain: false,
            pieces,
        }
    }

    /// The instruction lines of that text, without the directives or the labels.
    fn body(text: &str) -> Vec<&str> {
        text.lines()
            .filter(|line| line.starts_with('\t') && !line.trim_start().starts_with('.'))
            .map(|line| line.trim_start())
            .collect()
    }

    #[test]
    fn an_instruction_is_written_the_way_the_target_says_it_is() {
        let text = write(|func, names| {
            let block = func.create_block();
            let add = Opcode::new(names.intern("x64.add_rr_32"));
            func.build(block, add)
                .operand(Operand::write(Reg::physical(RAX), GPR))
                .operand(Operand::read(Reg::physical(RAX), GPR))
                .operand(Operand::read(Reg::physical(RCX), GPR))
                .finish();
        });
        // The source before the destination, which is the reverse of the operand vector, and the
        // first source not written at all, because it is the destination.
        assert_eq!(body(&text), ["addl\t%ecx, %eax"]);
    }

    #[test]
    fn an_opcode_the_machine_has_no_single_instruction_for_is_written_as_the_ones_it_has() {
        let text = write(|func, names| {
            let block = func.create_block();
            let cmp = Opcode::new(names.intern("x64.cmp_set_l_64"));
            func.build(block, cmp)
                .operand(Operand::write(Reg::physical(RAX), GPR))
                .operand(Operand::read(Reg::physical(RCX), GPR))
                .operand(Operand::read(Reg::physical(RDX), GPR))
                .finish();
        });
        // Two instructions, the comparison at the width it was asked for and the set at the width
        // a set is, which is the case that says why a width is a fact about an argument.
        assert_eq!(body(&text), ["cmpq\t%rdx, %rcx", "setl\t%al"]);
    }

    #[test]
    fn an_opcode_that_is_not_an_instruction_is_written_as_nothing() {
        let text = write(|func, names| {
            let block = func.create_block();
            let ret = Opcode::new(names.intern("x64.ret_val_32"));
            func.build(block, ret).operand(Operand::read(Reg::physical(RAX), GPR)).finish();
        });
        assert_eq!(body(&text), Vec::<&str>::new());
    }

    #[test]
    fn an_alignment_is_written_as_the_directive_that_asks_for_it() {
        let text = write(|func, names| {
            let block = func.create_block();
            let align = Opcode::new(names.intern("x64.align"));
            func.build(block, align).imm(32).finish();
        });
        // The boundary is a power here and a count of bytes in the machine IR, because the
        // assembler reads the one and a program writes the other. The fill is the one byte that
        // does nothing, so a jump that lands in the padding still arrives. A directive rather than
        // an instruction, which is why it is looked for in the whole text and not in the body.
        assert!(text.contains("\n\t.p2align\t5, 0x90\n"), "{text}");
        assert_eq!(body(&text), Vec::<&str>::new());
    }

    #[test]
    fn an_address_is_a_displacement_and_then_the_registers_it_names() {
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
        assert_eq!(body(&text), ["leaq\t-16(%rcx,%rdx,4), %rax"]);
    }

    #[test]
    fn an_address_in_a_thread_s_own_block_names_the_segment_and_no_register() {
        let text = write(|func, names| {
            let block = func.create_block();
            let load = Opcode::new(names.intern("x64.mov_rm_64"));
            func.build(block, load)
                .operand(Operand::write(Reg::physical(RAX), GPR))
                .mem(Mem::in_segment(rucc_target::Segment::Fs, 40))
                .finish();
        });
        // The first line of every function this compiler protects. No base and no index, because
        // where the block begins is something only the machine knows, and the segment written in
        // front of the constant rather than behind it, which is what an assembler reads.
        assert_eq!(body(&text), ["movq\t%fs:40, %rax"]);
    }

    #[test]
    fn the_touch_a_probing_prologue_writes_is_an_immediate_and_then_an_address() {
        let text = write(|func, names| {
            let block = func.create_block();
            let touch = Opcode::new(names.intern("x64.or_mi_8"));
            func.build(block, touch)
                .imm(0)
                .mem(Mem::at(Operand::read(Reg::physical(RSP), GPR)))
                .finish();
        });
        // The only instruction this compiler writes that has a number and an address and no
        // register of its own. An inclusive or of zero, so the byte it writes is the byte that was
        // there, which is what makes it safe on a page nothing has been put in yet.
        assert_eq!(body(&text), ["orb\t$0, (%rsp)"]);
    }

    #[test]
    fn an_address_with_nothing_but_a_symbol_in_it_is_relative_to_the_instruction_pointer() {
        let text = write(|func, names| {
            let block = func.create_block();
            let load = Opcode::new(names.intern("x64.mov_rm_64"));
            let global = names.intern("counter");
            func.build(block, load)
                .operand(Operand::write(Reg::physical(RAX), GPR))
                .mem(Mem::of(global))
                .finish();
        });
        assert_eq!(body(&text), ["movq\tcounter(%rip), %rax"]);
    }

    #[test]
    fn an_address_with_a_label_in_it_is_the_label_and_is_relative_as_well() {
        let text = write(|func, names| {
            let head = func.create_block();
            let there = func.create_block();
            let lea = Opcode::new(names.intern("x64.lea_64"));
            let jump = Opcode::new(names.intern("x64.jmp_reg"));
            func.build(head, lea)
                .operand(Operand::write(Reg::physical(RAX), GPR))
                .mem(Mem::block(there))
                .finish();
            func.build(head, jump).operand(Operand::read(Reg::physical(RAX), GPR)).finish();
            func.build(there, Opcode::new(names.intern("x64.ret"))).finish();
        });
        // The four bytes a symbol would leave, holding a distance the assembler works out for
        // itself rather than one a relocation asks the linker for, since both ends of it are in
        // the section being written.
        assert_eq!(body(&text), ["leaq\t.Lf_1(%rip), %rax", "jmp\t*%rax", "ret"]);
    }

    /// A function that jumps through a table of three cells to one of two returns, written out for
    /// that object format with its sections split up or not.
    fn switching(os: Os, sections: Sections) -> String {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let head = func.create_block();
        let first = func.create_block();
        let second = func.create_block();
        let lea = Opcode::new(names.intern("x64.lea_64"));
        let jmp = Opcode::new(names.intern("x64.jmp_reg"));
        func.build(head, lea)
            .operand(Operand::write(Reg::physical(RAX), GPR))
            .mem(Mem::table(0))
            .finish();
        let jump = func.build(head, jmp).operand(Operand::read(Reg::physical(RAX), GPR)).finish();
        func.succs_mut(head).push(rucc_mir::BlockCall::to(first));
        func.succs_mut(head).push(rucc_mir::BlockCall::to(second));
        func.build(first, Opcode::new(names.intern("x64.ret"))).finish();
        func.build(second, Opcode::new(names.intern("x64.ret"))).finish();
        func.tables.push(rucc_mir::Table { jump, cells: vec![0, 1, 0], absolute: false });
        let output = Output { sections, ..Output::default() };
        print(&[func], &Globals::default(), &[], &names, &target(os), true, output)
            .expect("a function that was allocated")
    }

    /// The lines from the one in front of the table's label to the one after its last cell.
    fn around_table(text: &str) -> Vec<&str> {
        let lines: Vec<&str> = text.lines().collect();
        let at = lines.iter().position(|line| *line == ".Lf_j0:").expect("a table");
        lines[at - 2..at + 5].to_vec()
    }

    #[test]
    fn a_jump_table_is_a_label_and_the_distance_to_each_block_from_it_in_rodata() {
        let text = switching(Os::Linux, Sections::default());
        assert_eq!(body(&text), ["leaq\t.Lf_j0(%rip), %rax", "jmp\t*%rax", "ret", "ret"]);
        assert_eq!(
            around_table(&text),
            [
                "\t.section\t.rodata",
                "\t.p2align\t2",
                ".Lf_j0:",
                "\t.long\t.Lf_1-.Lf_j0",
                "\t.long\t.Lf_2-.Lf_j0",
                "\t.long\t.Lf_1-.Lf_j0",
                "\t.text",
            ],
            "{text}"
        );
        // Back in the code before the function is closed, so that its size is still counted there.
        let closed = text.find("\t.size\tf").expect("a size");
        assert!(text[..closed].ends_with("\t.text\n\t.cfi_endproc\n"), "{text}");
    }

    #[test]
    fn a_jump_table_under_data_sections_goes_in_a_section_named_after_its_function() {
        let text = switching(Os::Linux, Sections { functions: true, data: true });
        assert_eq!(around_table(&text)[0], "\t.section\t.rodata.f,\"a\",@progbits", "{text}");
        assert_eq!(around_table(&text)[6], "\t.section\t.text.f,\"ax\",@progbits", "{text}");
        // Code sections alone leave the tables in the one `.rodata`, which is what gcc does.
        let text = switching(Os::Linux, Sections { functions: true, data: false });
        assert_eq!(around_table(&text)[0], "\t.section\t.rodata", "{text}");
        assert_eq!(around_table(&text)[6], "\t.section\t.text.f,\"ax\",@progbits", "{text}");
    }

    #[test]
    fn a_jump_table_on_windows_stays_after_the_code() {
        let text = switching(Os::Windows, Sections::default());
        assert!(!text.contains("rodata"), "{text}");
        let lines: Vec<&str> = text.lines().collect();
        let at = lines.iter().position(|line| *line == ".Lf_j0:").expect("a table");
        assert_eq!(lines[at - 2..=at], ["\tret", "\t.p2align\t2, 0x90", ".Lf_j0:"], "{text}");
    }

    #[test]
    fn an_address_that_reads_the_offset_table_says_so_on_the_symbol() {
        let text = write(|func, names| {
            let block = func.create_block();
            let load = Opcode::new(names.intern("x64.mov_rm_64"));
            let away = names.intern("away");
            func.build(block, load)
                .operand(Operand::write(Reg::physical(RAX), GPR))
                .mem(Mem::got(away))
                .finish();
        });
        // The same instruction and the same four bytes as the one above. What is different is
        // which relocation those four bytes take, and the suffix on the name is the only place
        // the assembler is told which.
        assert_eq!(body(&text), ["movq\taway@GOTPCREL(%rip), %rax"]);
    }

    #[test]
    fn an_address_that_reads_the_offset_of_a_thread_local_says_so_on_the_symbol_as_well() {
        let text = write(|func, names| {
            let block = func.create_block();
            let load = Opcode::new(names.intern("x64.mov_rm_64"));
            let away = names.intern("away");
            func.build(block, load)
                .operand(Operand::write(Reg::physical(RAX), GPR))
                .mem(Mem::thread(away))
                .finish();
        });
        // The third instruction that is the same instruction as the two above it. What comes back
        // this time is not an address at all: it is how far into a thread's own block the variable
        // sits, and what makes it an address is the addition that follows it.
        assert_eq!(body(&text), ["movq\taway@GOTTPOFF(%rip), %rax"]);
    }

    #[test]
    fn a_name_under_the_kernel_model_is_written_as_a_number() {
        let text = write(|func, names| {
            let block = func.create_block();
            let lea = Opcode::new(names.intern("x64.lea_64"));
            let load = Opcode::new(names.intern("x64.mov_rm_64"));
            let here = names.intern("here");
            let tab = names.intern("tab");
            func.build(block, lea)
                .operand(Operand::write(Reg::physical(RAX), GPR))
                .mem(Mem { disp: 4, ..Mem::absolute(here) })
                .finish();
            func.build(block, load)
                .operand(Operand::write(Reg::physical(RAX), GPR))
                .mem(Mem {
                    index: Some(Operand::read(Reg::physical(RCX), GPR)),
                    scale: 8,
                    ..Mem::absolute(tab)
                })
                .finish();
        });
        // What gcc writes for the kernel: the address as an immediate, and an index added to the
        // name with no `(%rip)`, both of which gas gives `R_X86_64_32S`.
        assert_eq!(body(&text), ["movq\t$here+4, %rax", "movq\ttab(,%rcx,8), %rax"]);
    }

    #[test]
    fn a_jump_goes_to_the_label_of_the_block_the_first_arm_names() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let first = func.create_block();
        let second = func.create_block();
        let jmp = Opcode::new(names.intern("x64.jmp"));
        func.build(first, jmp).finish();
        func.succs_mut(first).push(rucc_mir::BlockCall::to(second));
        let text = print(
            &[func],
            &Globals::default(),
            &[],
            &names,
            &target(Os::Linux),
            true,
            Output::default(),
        )
        .expect("a function of two blocks");
        assert!(text.contains("\tjmp\t.Lf_1\n"), "{text}");
        assert!(text.contains("\n.Lf_1:\n"), "{text}");
    }

    #[test]
    fn a_marked_listing_has_a_label_in_front_of_every_instruction_and_numbers_the_functions() {
        let mut names = Interner::new();
        let jmp = Opcode::new(names.intern("x64.jmp"));
        let mut funcs = Vec::new();
        for name in ["f", "g"] {
            let mut func = Func::new(names.intern(name));
            let first = func.create_block();
            let second = func.create_block();
            func.build(first, jmp).finish();
            func.succs_mut(first).push(rucc_mir::BlockCall::to(second));
            funcs.push(func);
        }
        let target = target(Os::Linux);
        let text = print_marked(
            &funcs,
            &Globals::default(),
            &[],
            &names,
            &target,
            true,
            Output::default(),
        )
        .expect("two functions");
        let inst = funcs[1].insts(funcs[1].blocks().next().unwrap()).next().unwrap();
        let label = mark(&target, 1, inst);
        assert!(label.starts_with(".L"), "{label}");
        assert!(text.contains(&format!("\n{label}:\n\tjmp\t.Lg_1\n")), "{text}");
        assert!(text.contains(&format!("\n{}:\n", mark(&target, 0, inst))), "{text}");
        // And none at all in the listing `-S` writes.
        let plain =
            print(&funcs, &Globals::default(), &[], &names, &target, true, Output::default())
                .expect("two functions");
        assert!(!plain.contains("rucc_row"), "{plain}");
    }

    #[test]
    fn a_symbol_is_spelled_the_way_the_object_format_spells_one() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        let call = Opcode::new(names.intern("x64.call"));
        let callee = names.intern("puts");
        func.build(block, call).symbol(callee).finish();

        let elf = print(
            std::slice::from_ref(&func),
            &Globals::default(),
            &[],
            &names,
            &target(Os::Linux),
            true,
            Output::default(),
        )
        .expect("elf");
        assert!(elf.contains("\tcall\tputs\n"), "{elf}");
        assert!(elf.contains("\n.Lf_0:\n"), "{elf}");

        // The underscore, which is the difference that would fail to link against every library
        // on an Apple machine rather than merely looking odd.
        let macho = print(
            &[func],
            &Globals::default(),
            &[],
            &names,
            &target(Os::Darwin),
            true,
            Output::default(),
        )
        .expect("mach-o");
        assert!(macho.contains("\tcall\t_puts\n"), "{macho}");
        assert!(macho.contains("\n_f:\n"), "{macho}");
        assert!(macho.contains("\nLf_0:\n"), "{macho}");
    }

    #[test]
    fn a_function_that_was_never_allocated_is_refused_rather_than_written_wrongly() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        let vreg = func.new_vreg(GPR);
        let neg = Opcode::new(names.intern("x64.neg_r_32"));
        func.build(block, neg).operand(Operand::write(vreg, GPR)).finish();
        let error = print(
            &[func],
            &Globals::default(),
            &[],
            &names,
            &target(Os::Linux),
            true,
            Output::default(),
        )
        .expect_err("a virtual register");
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
        let error = print(
            &[func],
            &Globals::default(),
            &[],
            &names,
            &target(Os::Linux),
            true,
            Output::default(),
        )
        .expect_err("no such instruction");
        assert_eq!(
            error,
            Error::Opcode { func: "f".to_owned(), opcode: "x64.frobnicate".to_owned() }
        );
    }

    #[test]
    fn a_function_no_other_file_can_see_is_not_announced_to_the_linker() {
        let mut names = Interner::new();
        let mut hidden = Func::new(names.intern("hidden"));
        hidden.binding = rucc_mir::Binding::Local;
        hidden.create_block();
        let text = print(
            &[hidden],
            &Globals::default(),
            &[],
            &names,
            &target(Os::Linux),
            true,
            Output::default(),
        )
        .expect("elf");
        // Still a symbol, and still at the alignment a function gets, because a local name is one
        // the linker keeps and does not let another file reach.
        assert!(text.contains("\nhidden:\n"), "{text}");
        assert!(text.contains("\t.type\thidden, @function\n"), "{text}");
        // What two files each defining their own `static helper` come down to.
        assert!(!text.contains(".globl"), "{text}");
    }

    #[test]
    fn a_function_that_may_lose_to_another_definition_is_written_weak() {
        let mut names = Interner::new();
        let mut shared = Func::new(names.intern("shared"));
        shared.binding = rucc_mir::Binding::Weak;
        shared.create_block();
        let text = print(
            &[shared],
            &Globals::default(),
            &[],
            &names,
            &target(Os::Linux),
            true,
            Output::default(),
        )
        .expect("elf");
        assert!(text.contains("\t.weak\tshared\n"), "{text}");
        assert!(!text.contains(".globl"), "{text}");
    }

    /// The whole of what an assembler is told about one, and none of what it works out itself:
    /// the type and the size of the new name come from the old one, so they are not written
    /// again. gcc 16 writes exactly these two lines for the same input.
    #[test]
    fn a_second_name_is_a_binding_and_a_set_and_nothing_else() {
        let names = Interner::new();
        let aliases = [
            Alias {
                name: "b".to_owned(),
                target: "a".to_owned(),
                binding: Binding::Global,
                visibility: Visibility::Default,
                ifunc: false,
            },
            Alias {
                name: "c".to_owned(),
                target: "a".to_owned(),
                binding: Binding::Weak,
                visibility: Visibility::Default,
                ifunc: false,
            },
            Alias {
                name: "d".to_owned(),
                target: "a".to_owned(),
                binding: Binding::Local,
                visibility: Visibility::Default,
                ifunc: false,
            },
        ];
        let vars = vec![var("a", Place::Written, vec![Piece::Scalar(vec![1, 0, 0, 0])])];
        let text = print(
            &[],
            &Globals { vars, ..Globals::default() },
            &aliases,
            &names,
            &target(Os::Linux),
            true,
            Output::default(),
        )
        .expect("a machine with a writer");
        assert!(text.contains("\t.globl\tb\n\t.set\tb,a\n"), "{text}");
        assert!(text.contains("\t.weak\tc\n\t.set\tc,a\n"), "{text}");
        // A local one is a name no directive announces, which is still an entry in the symbol
        // table and is what a `static` alias comes down to.
        assert!(text.contains("\t.set\td,a\n"), "{text}");
        assert!(!text.contains("\t.type\tb"), "the type comes from what it points at: {text}");
        assert!(!text.contains("\t.size\tb"), "and so does the size: {text}");
        // Four bytes of image and not sixteen, since three more names for one variable are three
        // more names and not three more variables.
        assert_eq!(text.matches(".long\t1").count(), 1, "{text}");
    }

    /// gcc writes each alias just after what it names, so the names reach the symbol table in the
    /// order the targets are written and not the order the aliases were declared in. The kernel's
    /// modpost reads `MODULE_DEVICE_TABLE` aliases in that order.
    #[test]
    fn a_second_name_follows_what_it_names() {
        let names = Interner::new();
        let alias = |name: &str, target: &str| Alias {
            name: name.to_owned(),
            target: target.to_owned(),
            binding: Binding::Local,
            visibility: Visibility::Default,
            ifunc: false,
        };
        let aliases = [alias("for_b", "b"), alias("for_a", "a"), alias("again", "for_a")];
        let vars = vec![
            var("a", Place::Written, vec![Piece::Scalar(vec![1, 0, 0, 0])]),
            var("b", Place::Written, vec![Piece::Scalar(vec![2, 0, 0, 0])]),
        ];
        let text = print(
            &[],
            &Globals { vars, ..Globals::default() },
            &aliases,
            &names,
            &target(Os::Linux),
            true,
            Output::default(),
        )
        .expect("a machine with a writer");
        let at = |line: &str| text.find(line).unwrap_or_else(|| panic!("{line}: {text}"));
        assert!(at(".long\t1") < at("\t.set\tfor_a,a\n"), "{text}");
        assert!(at("\t.set\tfor_a,a\n") < at("\t.set\tagain,for_a\n"), "{text}");
        assert!(at("\t.set\tagain,for_a\n") < at(".long\t2"), "{text}");
        assert!(at(".long\t2") < at("\t.set\tfor_b,b\n"), "{text}");
    }

    /// An ifunc is the one alias that says a type, between the binding and the `.set`, which is
    /// where gcc 16 writes it for a function with `target_clones`.
    #[test]
    fn an_ifunc_says_its_type_before_it_is_set() {
        let names = Interner::new();
        let aliases = [Alias {
            name: "f".to_owned(),
            target: "f.resolver".to_owned(),
            binding: Binding::Global,
            visibility: Visibility::Default,
            ifunc: true,
        }];
        let text = print(
            &[],
            &Globals::default(),
            &aliases,
            &names,
            &target(Os::Linux),
            true,
            Output::default(),
        )
        .expect("a machine with a writer");
        let want = "\t.globl\tf\n\t.type\tf, @gnu_indirect_function\n\t.set\tf,f.resolver\n";
        assert!(text.contains(want), "{text}");
    }

    #[test]
    fn a_variable_is_a_section_a_name_and_the_bytes_between_them() {
        let text = data(
            vec![var("counter", Place::Written, vec![Piece::Scalar(vec![42, 0, 0, 0])])],
            Os::Linux,
        );
        assert!(text.contains("\t.data\n"), "{text}");
        assert!(text.contains("\t.globl\tcounter\n"), "{text}");
        assert!(text.contains("\t.p2align\t2\n"), "{text}");
        assert!(text.contains("\t.type\tcounter, @object\n"), "{text}");
        // The number at the width it is, rather than the four bytes it is made of, because a
        // listing is a thing to read and the bytes are the object's business.
        assert!(text.contains("\ncounter:\n\t.long\t42\n"), "{text}");
        assert!(text.contains("\t.size\tcounter, .-counter\n"), "{text}");
    }

    /// The flag and the type are the whole of what makes it thread-local in a listing, and they
    /// are what gcc 16.2.0 writes for `_Thread_local int counter = 42;`.
    #[test]
    fn a_thread_local_variable_is_a_section_with_the_flag_on_it_and_a_type_of_its_own() {
        let text = data(
            vec![var(
                "counter",
                Place::Thread { zero: false },
                vec![Piece::Scalar(vec![42, 0, 0, 0])],
            )],
            Os::Linux,
        );
        assert!(text.contains("\t.section\t.tdata,\"awT\",@progbits\n"), "{text}");
        assert!(text.contains("\t.type\tcounter, @tls_object\n"), "{text}");
        assert!(text.contains("\ncounter:\n\t.long\t42\n"), "{text}");
    }

    /// The other half of the pair, which is `.bss` to the one above's `.data`.
    #[test]
    fn a_thread_local_variable_with_no_image_to_carry_goes_in_the_section_that_carries_none() {
        let text = data(
            vec![var("counter", Place::Thread { zero: true }, vec![Piece::Zero(4)])],
            Os::Linux,
        );
        assert!(text.contains("\t.section\t.tbss,\"awT\",@nobits\n"), "{text}");
        assert!(text.contains("\t.type\tcounter, @tls_object\n"), "{text}");
        assert!(text.contains("\ncounter:\n\t.space\t4\n"), "{text}");
    }

    /// What clang writes for `__thread int counter = 42;` on either Apple architecture: the image
    /// under a name of its own, and the name the program uses on a descriptor that points at it.
    #[test]
    fn a_thread_local_variable_on_mach_o_is_an_image_and_a_descriptor() {
        let text = data(
            vec![var(
                "counter",
                Place::Thread { zero: false },
                vec![Piece::Scalar(vec![42, 0, 0, 0])],
            )],
            Os::Darwin,
        );
        let image = "\t.section\t__DATA,__thread_data,thread_local_regular\n\t.p2align\t2\n\
                     _counter$tlv$init:\n\t.long\t42\n";
        assert!(text.contains(image), "{text}");
        let descriptor = "\t.section\t__DATA,__thread_vars,thread_local_variables\n\
                          \t.globl\t_counter\n\t.p2align\t3\n_counter:\n\
                          \t.quad\t__tlv_bootstrap\n\t.quad\t0\n\t.quad\t_counter$tlv$init\n";
        assert!(text.contains(descriptor), "{text}");
        let zero = data(
            vec![var("counter", Place::Thread { zero: true }, vec![Piece::Zero(4)])],
            Os::Darwin,
        );
        assert!(zero.contains("\t.tbss\t_counter$tlv$init,4,2\n"), "{zero}");
        assert!(zero.contains(descriptor), "{zero}");
    }

    #[test]
    fn a_variable_no_other_file_can_see_is_not_announced_to_the_linker() {
        let mut hidden = var("hidden", Place::Zero, vec![Piece::Zero(4)]);
        hidden.binding = Binding::Local;
        let text = data(vec![hidden], Os::Linux);
        assert!(text.contains("\t.bss\n"), "{text}");
        assert!(text.contains("\nhidden:\n\t.space\t4\n"), "{text}");
        // The whole of what `static` at file scope means, and the one thing a reader would not
        // notice missing until two files each defined their own and the linker took one.
        assert!(!text.contains(".globl"), "{text}");
    }

    #[test]
    fn a_tentative_definition_is_a_request_rather_than_a_section_and_a_label() {
        let text = data(vec![var("x", Place::Merged, vec![Piece::Zero(4)])], Os::Linux);
        assert_eq!(text.lines().find(|line| line.contains(".comm")), Some("\t.comm\tx,4,4"));
        assert!(!text.contains("\nx:\n"), "nothing here says where it is: {text}");
    }

    /// The listing half of `-ffunction-sections`, which is the flag that makes `--gc-sections` able
    /// to drop anything: a linker can leave out a section nothing reaches and cannot leave out half
    /// of one.
    ///
    /// The empty `.text` at the top stays. It is what the file opens with either way, gcc 16 writes
    /// one under the flag too, and a section with nothing in it costs a header and confuses nobody.
    #[test]
    fn every_function_gets_a_section_of_its_own_when_that_is_what_was_asked_for() {
        let text = split_code("first", "second", Os::Linux);
        assert!(text.starts_with("\t.text\n"), "{text}");
        assert!(text.contains("\t.section\t.text.first,\"ax\",@progbits\n"), "{text}");
        assert!(text.contains("\t.section\t.text.second,\"ax\",@progbits\n"), "{text}");
        // In front of the alignment and the name rather than after them, since the padding belongs
        // to the section the function is in and a label in the wrong section is a wrong address.
        let opened = text.find(".section\t.text.first").expect("a section");
        assert!(opened < text.find("\nfirst:\n").expect("a label"), "{text}");
        // And one text section when nothing asked, which is the default.
        let plain = write(|_, _| {});
        assert!(!plain.contains(".text."), "{plain}");
    }

    /// Mach-O takes the flag and writes what it wrote before, because every Mach-O object ends
    /// with `.subsections_via_symbols` and so already tells the linker it may split a section at
    /// each symbol and drop the parts nothing reaches. Clang does the same on an Apple target.
    #[test]
    fn a_format_that_already_lets_the_linker_split_a_section_is_not_asked_to_split_it_again() {
        let text = split_code("first", "second", Os::Darwin);
        assert!(text.contains("\t.subsections_via_symbols\n"), "{text}");
        assert_eq!(text.matches(".section").count(), 1, "the one it opens with: {text}");
        let vars = vec![var("counter", Place::Written, vec![Piece::Scalar(vec![1, 0, 0, 0])])];
        assert_eq!(split(vars.clone(), Os::Darwin), data(vars, Os::Darwin));
    }

    /// The listing half of `-fdata-sections`, where the name of the section is the name of the one
    /// it came out of with the variable's name after it. That is what gcc writes, and the part in
    /// front of the dot is what a linker script and `--gc-sections` both match on.
    #[test]
    fn every_variable_gets_a_section_named_after_it_when_that_is_what_was_asked_for() {
        let vars = vec![
            var("g", Place::Written, vec![Piece::Scalar(vec![1, 0, 0, 0])]),
            var("z", Place::Zero, vec![Piece::Zero(4)]),
            var("r", Place::ReadOnly, vec![Piece::Scalar(vec![3, 0, 0, 0])]),
        ];
        let text = split(vars.clone(), Os::Linux);
        assert!(text.contains("\t.section\t.data.g,\"aw\"\n\t.globl\tg\n"), "{text}");
        assert!(text.contains("\t.section\t.bss.z,\"aw\",@nobits\n"), "{text}");
        assert!(text.contains("\t.section\t.rodata.r,\"a\"\n"), "{text}");
        // Everything else about the variable is what it was: splitting moves which section header
        // the name is in and must not change the image, the size or who can see it.
        assert!(text.contains("\ng:\n\t.long\t1\n"), "{text}");
        assert!(text.contains("\t.size\tg, .-g\n"), "{text}");
        assert!(text.contains("\t.space\t4\n"), "{text}");
        // And the flag reaches the data without reaching the code, since gcc has two flags and a
        // build that asked for one of them measured something.
        assert!(!text.contains(".text."), "{text}");
        let plain = data(vars, Os::Linux);
        assert!(plain.contains("\t.data\n") && plain.contains("\t.bss\n"), "{plain}");
        assert!(!plain.contains(".data.g"), "{plain}");
    }

    #[test]
    fn the_object_format_decides_how_a_variable_is_written_as_much_as_a_function() {
        let text = data(vec![var("x", Place::Zero, vec![Piece::Zero(4)])], Os::Darwin);
        // Mach-O has no way to put bytes in its zero filled section, so a variable that goes
        // there is asked for by size the way a tentative definition is on every format.
        // The directive is also the definition, so the binding goes above it or the variable is
        // one no other file can find.
        assert!(text.contains("\t.globl\t_x\n\t.zerofill\t__DATA,__bss,_x,4,2\n"), "{text}");
        let read_only = data(vec![var("x", Place::ReadOnly, vec![Piece::Zero(4)])], Os::Darwin);
        assert!(read_only.contains("\t.section\t__TEXT,__const\n"), "{read_only}");
        assert!(read_only.contains("\n_x:\n"), "the underscore, without which nothing links");
    }

    #[test]
    fn a_run_of_bytes_is_written_so_that_it_reads_back_as_the_same_bytes() {
        let bytes = Piece::Bytes(b"a\"b\\\n\0\x801".to_vec());
        let text = data(vec![var("s", Place::ReadOnly, vec![bytes])], Os::Linux);
        // Three octal digits every time, so that the digit after an escape is not read as part
        // of it, and the quote and the backslash escaped so the string ends where it should.
        assert!(text.contains("\t.ascii\t\"a\\\"b\\\\\\012\\000\\2001\"\n"), "{text}");
    }

    #[test]
    fn the_address_of_a_name_in_an_image_is_written_as_the_name() {
        let addr = Piece::Addr { symbol: "y".to_owned(), addend: 16, bytes: 8 };
        let text = data(vec![var("p", Place::Written, vec![addr])], Os::Linux);
        assert!(text.contains("\np:\n\t.quad\ty+16\n"), "{text}");
    }

    #[test]
    fn a_distance_in_an_image_is_written_as_the_name_less_where_it_is() {
        let away = Piece::Away { symbol: "y".to_owned(), addend: 0 };
        let text = data(vec![var("d", Place::ReadOnly, vec![away])], Os::Linux);
        assert!(text.contains("\nd:\n\t.long\ty - .\n"), "{text}");

        let away = Piece::Away { symbol: "y".to_owned(), addend: -3 };
        let text = data(vec![var("d", Place::ReadOnly, vec![away])], Os::Linux);
        assert!(text.contains("\nd:\n\t.long\ty-3 - .\n"), "{text}");
    }

    #[test]
    fn a_distance_between_two_labels_is_written_as_one_less_the_other() {
        let piece = |addend, bytes| Piece::Apart {
            to: ".Llbl.1".to_owned(),
            from: ".Llbl.0".to_owned(),
            addend,
            bytes,
        };
        let text = data(vec![var("b", Place::ReadOnly, vec![piece(0, 4)])], Os::Linux);
        assert!(text.contains("\nb:\n\t.long\t.Llbl.1-.Llbl.0\n"), "{text}");

        let text = data(vec![var("b", Place::ReadOnly, vec![piece(-2, 2)])], Os::Linux);
        assert!(text.contains("\nb:\n\t.short\t.Llbl.1-.Llbl.0-2\n"), "{text}");
    }

    /// What gcc writes for a variable a Windows file reads and only declares: the pointer in a
    /// section of its own, named after it, which the linker keeps one copy of.
    #[test]
    fn a_pointer_to_a_variable_elsewhere_is_written_the_way_clang_writes_it() {
        let mut globals = Globals::default();
        globals.pointers([(".refptr.environ".to_owned(), "environ".to_owned())]);
        let names = Interner::new();
        let text = print(&[], &globals, &[], &names, &target(Os::Windows), true, Output::default())
            .expect("a machine with a writer");
        let expected = "\t.section\t.rdata$.refptr.environ,\"dr\"\n\t.linkonce\tdiscard\n\
                        \t.globl\t.refptr.environ\n\t.p2align\t3\n.refptr.environ:\n\
                        \t.quad\tenviron\n";
        assert!(text.contains(expected), "{text}");
    }

    /// What clang writes for `dllexport`: every name as a linker option in `.drectve`, the
    /// functions first, and a variable marked as data. A hidden definition is an option there too.
    #[test]
    fn a_name_offered_to_other_dlls_is_an_option_in_the_directive_section() {
        let exports = vec![
            rucc_object::Export { name: "offered".to_owned(), kind: rucc_object::Offer::Function },
            rucc_object::Export { name: "count".to_owned(), kind: rucc_object::Offer::Variable },
            rucc_object::Export { name: "kept".to_owned(), kind: rucc_object::Offer::Hidden },
        ];
        let names = Interner::new();
        let text = print(
            &[],
            &Globals { exports, ..Globals::default() },
            &[],
            &names,
            &target(Os::Windows),
            true,
            Output::default(),
        )
        .expect("a machine with a writer");
        let expected = "\t.section\t.drectve,\"yni\"\n\t.ascii\t\" -export:offered\"\n\
                        \t.ascii\t\" -export:count,data\"\n\t.ascii\t\" -exclude-symbols:kept\"\n";
        assert!(text.contains(expected), "{text}");
        let none = data(Vec::new(), Os::Windows);
        assert!(!none.contains(".drectve"), "{none}");
    }

    #[test]
    fn a_machine_with_no_writer_here_is_said_so_rather_than_written_as_x86_64() {
        let names = Interner::new();
        let riscv = TargetInfo::new(Triple::new(Arch::Riscv64, Os::Linux, Env::Gnu));
        let error = print(&[], &Globals::default(), &[], &names, &riscv, true, Output::default())
            .expect_err("no writer");
        assert!(matches!(error, Error::Machine { .. }), "{error:?}");
    }

    /// One i386 function of one block, with those instructions in it, written out.
    fn write_i386(build: impl FnOnce(&mut Func, &mut Interner)) -> Result<String, Error> {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        build(&mut func, &mut names);
        let target = TargetInfo::new(Triple::new(Arch::X86, Os::Linux, Env::Gnu));
        print(&[func], &Globals::default(), &[], &names, &target, true, Output::default())
    }

    #[test]
    fn an_i386_instruction_names_its_registers_and_its_addresses_at_thirty_two_bits() {
        use rucc_target::x86::{EAX, EBP, ECX, EDX};
        let text = write_i386(|func, names| {
            let block = func.create_block();
            func.build(block, Opcode::new(names.intern("x64.push_32")))
                .operand(Operand::read(Reg::physical(EBP), GPR))
                .finish();
            func.build(block, Opcode::new(names.intern("x64.lea_32")))
                .operand(Operand::write(Reg::physical(EAX), GPR))
                .mem(
                    Mem::at(Operand::read(Reg::physical(ECX), GPR))
                        .indexed(Operand::read(Reg::physical(EDX), GPR), 4)
                        .plus(-16),
                )
                .finish();
            func.build(block, Opcode::new(names.intern("x64.call_reg")))
                .operand(Operand::read(Reg::physical(EAX), GPR))
                .finish();
            func.build(block, Opcode::new(names.intern("x64.pop_32")))
                .operand(Operand::write(Reg::physical(EBP), GPR))
                .finish();
        })
        .expect("an i386 function");
        assert_eq!(
            body(&text),
            ["pushl\t%ebp", "leal\t-16(%ecx,%edx,4), %eax", "call\t*%eax", "popl\t%ebp"]
        );
    }

    #[test]
    fn an_i386_address_with_nothing_but_a_symbol_in_it_is_the_symbol() {
        use rucc_target::x86::{EAX, EBX};
        let text = write_i386(|func, names| {
            let block = func.create_block();
            let load = Opcode::new(names.intern("x64.mov_rm_32"));
            let counter = names.intern("counter");
            let away = names.intern("away");
            func.build(block, load)
                .operand(Operand::write(Reg::physical(EAX), GPR))
                .mem(Mem::of(counter))
                .finish();
            // The table's address in a register, which is how position independent code on this
            // machine reaches a slot, and a thread's offset read with and without it.
            let table = Some(Operand::read(Reg::physical(EBX), GPR));
            func.build(block, load)
                .operand(Operand::write(Reg::physical(EAX), GPR))
                .mem(Mem { base: table, ..Mem::got(away) })
                .finish();
            func.build(block, load)
                .operand(Operand::write(Reg::physical(EAX), GPR))
                .mem(Mem { base: table, ..Mem::thread(away) })
                .finish();
            func.build(block, load)
                .operand(Operand::write(Reg::physical(EAX), GPR))
                .mem(Mem::thread(away))
                .finish();
        })
        .expect("an i386 function");
        assert_eq!(
            body(&text),
            [
                "movl\tcounter, %eax",
                "movl\taway@GOT(%ebx), %eax",
                "movl\taway@GOTNTPOFF(%ebx), %eax",
                "movl\taway@INDNTPOFF, %eax",
            ]
        );
    }

    #[test]
    fn i386_position_independent_code_counts_from_the_table_and_calls_through_its_entries() {
        use rucc_mir::Flags;
        use rucc_target::x86::{EAX, EBX, ECX};
        let text = write_i386(|func, names| {
            let block = func.create_block();
            let after = func.create_block();
            let lea = Opcode::new(names.intern("x64.lea_32"));
            let load = Opcode::new(names.intern("x64.mov_rm_32"));
            let table = Operand::read(Reg::physical(EBX), GPR);
            func.build(block, lea)
                .operand(Operand::write(Reg::physical(EAX), GPR))
                .mem(Mem::got_off(table, names.intern("near")))
                .finish();
            func.build(block, load)
                .operand(Operand::write(Reg::physical(EAX), GPR))
                .mem(
                    Mem::of(names.intern("arr"))
                        .from_table(table)
                        .indexed(Operand::read(Reg::physical(ECX), GPR), 4)
                        .plus(8),
                )
                .finish();
            func.build(block, lea)
                .operand(Operand::write(Reg::physical(EAX), GPR))
                .mem(Mem::block(after).from_table(table))
                .finish();
            // A slot stays a slot when it is counted from the register.
            func.build(block, load)
                .operand(Operand::write(Reg::physical(EAX), GPR))
                .mem(Mem::got(names.intern("away")).from_table(table))
                .finish();
            func.build(block, Opcode::new(names.intern("x64.call")))
                .symbol(names.intern("strlen"))
                .flags(Flags::PLT)
                .operand(Operand::read(Reg::physical(EBX), GPR))
                .finish();
            func.build(after, Opcode::new(names.intern("x64.ret"))).finish();
        })
        .expect("an i386 function");
        let body = body(&text);
        assert_eq!(
            body[..2],
            ["leal\tnear@GOTOFF(%ebx), %eax", "movl\tarr@GOTOFF+8(%ebx,%ecx,4), %eax"]
        );
        assert!(
            body[2].starts_with("leal\t.L") && body[2].ends_with("@GOTOFF(%ebx), %eax"),
            "{text}"
        );
        assert_eq!(body[3..5], ["movl\taway@GOT(%ebx), %eax", "call\tstrlen@PLT"]);
    }

    #[test]
    fn an_i386_listing_refuses_a_register_the_machine_does_not_have() {
        use rucc_target::x86::{EAX, ECX};
        let wide = write_i386(|func, names| {
            let block = func.create_block();
            func.build(block, Opcode::new(names.intern("x64.add_rr_64")))
                .operand(Operand::write(Reg::physical(EAX), GPR))
                .operand(Operand::read(Reg::physical(EAX), GPR))
                .operand(Operand::read(Reg::physical(ECX), GPR))
                .finish();
        });
        assert!(matches!(wide, Err(Error::Encode { .. })), "{wide:?}");
        let high = write_i386(|func, names| {
            let block = func.create_block();
            func.build(block, Opcode::new(names.intern("x64.mov_rr_32")))
                .operand(Operand::write(Reg::physical(EAX), GPR))
                .operand(Operand::read(Reg::physical(x86_64::R8), GPR))
                .finish();
        });
        assert!(matches!(high, Err(Error::Encode { .. })), "{high:?}");
    }

    /// One AArch64 function of one block, with those instructions in it, written out.
    fn write_a64(build: impl FnOnce(&mut Func, &mut Interner)) -> Result<String, Error> {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        build(&mut func, &mut names);
        let target = TargetInfo::new(Triple::new(Arch::Aarch64, Os::Linux, Env::Gnu));
        print(&[func], &Globals::default(), &[], &names, &target, true, Output::default())
    }

    #[test]
    fn an_aarch64_instruction_is_written_the_way_its_own_table_says() {
        use rucc_target::aarch64::{self, x};
        let text = write_a64(|func, names| {
            let block = func.create_block();
            let add = Opcode::new(names.intern("a64.add_rr_32"));
            func.build(block, add)
                .operand(Operand::write(Reg::physical(x(0)), aarch64::GPR))
                .operand(Operand::read(Reg::physical(x(1)), aarch64::GPR))
                .operand(Operand::read(Reg::physical(x(2)), aarch64::GPR))
                .finish();
            let load = Opcode::new(names.intern("a64.ldr_64"));
            let base = Operand::read(Reg::physical(aarch64::SP), aarch64::GPR);
            func.build(block, load)
                .operand(Operand::write(Reg::physical(x(3)), aarch64::GPR))
                .mem(Mem::at(base).plus(16))
                .finish();
        })
        .expect("an allocated function");
        // Destination first, which is the order the operands are in already, and the stack
        // pointer as `sp` because 31 in a base is never the zero register.
        assert_eq!(body(&text), ["add w0, w1, w2", "ldr x3, [sp, #16]"]);
        // Padded by the assembler's own `nop`, since `0x90` is not an instruction here.
        assert!(text.contains("\t.p2align\t4\n"), "{text}");
        assert!(!text.contains("0x90"), "{text}");
    }

    #[test]
    fn an_aarch64_register_left_virtual_is_refused() {
        let error = write_a64(|func, names| {
            let block = func.create_block();
            let mov = Opcode::new(names.intern("a64.mov_rr_64"));
            let class = aarch64::GPR;
            let v0 = func.new_vreg(class);
            let v1 = func.new_vreg(class);
            func.build(block, mov)
                .operand(Operand::write(v0, class))
                .operand(Operand::read(v1, class))
                .finish();
        })
        .expect_err("a register was never allocated");
        assert!(matches!(error, Error::Virtual { .. }), "{error:?}");
    }

    #[test]
    fn an_aarch64_access_through_an_index_shifts_it_by_the_size() {
        use rucc_target::aarch64::{self, x};
        let text = write_a64(|func, names| {
            let block = func.create_block();
            let store = Opcode::new(names.intern("a64.str_32"));
            let base = Operand::read(Reg::physical(x(0)), aarch64::GPR);
            let index = Operand::read(Reg::physical(x(2)), aarch64::GPR);
            func.build(block, store)
                .operand(Operand::read(Reg::physical(x(3)), aarch64::GPR))
                .mem(Mem::at(base).indexed(index, 4))
                .finish();
            let load = Opcode::new(names.intern("a64.ldr_64"));
            func.build(block, load)
                .operand(Operand::write(Reg::physical(x(1)), aarch64::GPR))
                .mem(Mem::at(base).indexed(index, 1))
                .finish();
        })
        .expect("an allocated function");
        assert_eq!(body(&text), ["str w3, [x0, x2, lsl #2]", "ldr x1, [x0, x2]"]);
    }

    #[test]
    fn an_aarch64_offset_the_encoder_refuses_is_not_written() {
        use rucc_target::aarch64::{self, x};
        let error = write_a64(|func, names| {
            let block = func.create_block();
            let load = Opcode::new(names.intern("a64.ldr_64"));
            let base = Operand::read(Reg::physical(x(0)), aarch64::GPR);
            func.build(block, load)
                .operand(Operand::write(Reg::physical(x(1)), aarch64::GPR))
                .mem(Mem::at(base).plus(1 << 20))
                .finish();
        })
        .expect_err("an offset no load can hold");
        assert!(matches!(error, Error::Encode { .. }), "{error:?}");
    }

    #[test]
    fn an_opcode_aarch64_does_not_have_is_refused_rather_than_written_as_x86() {
        let error = write_a64(|func, names| {
            let block = func.create_block();
            func.build(block, Opcode::new(names.intern("x64.ret"))).finish();
        })
        .expect_err("not an AArch64 opcode");
        assert!(matches!(error, Error::Opcode { .. }), "{error:?}");
    }

    /// Three empty AArch64 functions, the middle one carrying the extensions `middle` says, in
    /// a unit built for `unit`, written out.
    fn extensions(unit: Isa, middle: Option<Isa>, arch: Arch) -> String {
        let mut names = Interner::new();
        let mut funcs = Vec::new();
        for (name, target) in [("before", None), ("middle", middle), ("after", None)] {
            let mut func = Func::new(names.intern(name));
            func.target = target;
            func.create_block();
            funcs.push(func);
        }
        let target = TargetInfo::new(Triple::new(arch, Os::Linux, Env::Gnu));
        let output = Output { isa: unit, ..Output::default() };
        print(&funcs, &Globals::default(), &[], &names, &target, true, output).expect("a listing")
    }

    /// tamnd/rucc#2304. A unit built for the CRC32 extension says so at the top, so that an
    /// assembler reading it takes the `crc32` instructions `<arm_acle.h>` writes, and a function
    /// built for it in a unit that is not says so around itself and puts the unit back after.
    #[test]
    fn the_assembler_is_told_about_the_crc_extension_where_it_is_on() {
        let crc = Isa::aarch64_march("armv8-a+crc");
        let plain = Isa::aarch64_march("armv8-a");
        let unit = extensions(crc, None, Arch::Aarch64);
        assert!(unit.starts_with("\t.arch_extension\tcrc\n"), "{unit}");
        assert_eq!(unit.matches(".arch_extension").count(), 1, "{unit}");
        let text = extensions(plain, Some(crc), Arch::Aarch64);
        let on = text.find("\t.arch_extension\tcrc\n").expect("switched on");
        let off = text.find("\t.arch_extension\tnocrc\n").expect("switched off");
        let before = text.find("\nbefore:").expect("before");
        let middle = text.find("\nmiddle:").expect("middle");
        let after = text.find("\nafter:").expect("after");
        assert!(before < on && on < middle && middle < off && off < after, "{text}");
        assert_eq!(text.matches(".arch_extension").count(), 2, "{text}");
        // And the other way round for a function that takes it away.
        let text = extensions(crc, Some(plain), Arch::Aarch64);
        let off = text.find("\t.arch_extension\tnocrc\n").expect("switched off");
        let on = text.rfind("\t.arch_extension\tcrc\n").expect("switched back on");
        assert!(off < text.find("\nmiddle:").expect("middle") && off < on, "{text}");
        // Nothing for a unit and functions that agree, and nothing on x86-64.
        let text = extensions(plain, Some(plain), Arch::Aarch64);
        assert!(!text.contains(".arch"), "{text}");
        assert!(!extensions(crc, Some(plain), Arch::X86_64).contains(".arch"));
    }

    #[test]
    fn the_head_of_a_loop_is_asked_to_stay_inside_one_line() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        func.create_block();
        let head = func.create_block();
        let jmp = Opcode::new(names.intern("x64.jmp"));
        func.build(head, jmp).finish();
        func.succs_mut(head).push(rucc_mir::BlockCall::to(head));
        func.heads = vec![head];
        let text = print(
            &[func],
            &Globals::default(),
            &[],
            &names,
            &target(Os::Linux),
            true,
            Output::default(),
        )
        .expect("a function with a loop in it");
        // The loop is the jump back to itself, five bytes, so it crosses a line only when it
        // starts in the last four bytes of one.
        let wanted = "\n.Lf_0:\n\t.p2align\t6,,4\n.Lf_1:\n";
        assert!(text.contains(wanted), "{text}");
    }
}
