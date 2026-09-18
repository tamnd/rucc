//! The frame rules a function carries, as the bytes of an unwind table.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.4.
//!
//! An unwinder is handed a return address and has to answer two questions about the function it
//! landed in: where the frame it is standing in ends, and where the callee saved registers went.
//! The answer is a table indexed by address, and it is read by everything that walks a stack, which
//! is `backtrace`, a C++ exception thrown through a C callback, `pthread_cancel`, a profiler
//! sampling with `--call-graph dwarf`, and a debugger once the frame pointer is gone.
//!
//! # What a record is
//!
//! A row per address, in principle, and in practice a program that builds the rows: start from the
//! rules the header gives, walk forward to the address wanted, and apply what is written along the
//! way. The rules only change where an instruction changes them, so a function is a handful of ops
//! with the distance from the last one in front of each.
//!
//! One header, called a CIE, holding what every function on the target starts out with. On x86-64
//! that is the state at the instruction a call lands on: the frame ends eight bytes above the stack
//! pointer, because the call pushed a return address, and that return address is the word below the
//! end. Then one record per function, called an FDE, saying which function it is, how long the
//! function is, and what changed inside it.
//!
//! # The other table
//!
//! Windows asks the same two questions and reads a different answer. There is no program per
//! function there: a row of three addresses says where the function is and where its description
//! is, and the description is a fixed list of codes, each one saying that the prologue pushed a
//! register, took so much frame, or put a register in a slot. A prologue whose shape is not on that
//! list has no spelling at all, which is the one real difference between the two formats and is why
//! this half can refuse and the DWARF half cannot.
//!
//! Both are built from the same rows, because what the rows say is what each instruction of the
//! prologue did to the frame and that is the question both tables answer.
//!
//! # What is here and what is not
//!
//! Here: the encoding, which is the platform's. Not here: where the sections go and what their
//! flags are, which is the object writer's, and which rows a function has, which is the code
//! generator's because the prologue is the only thing that knows what it did. What the DWARF header
//! says is read out of the calling convention rather than written down again, since the state a
//! call leaves behind is the same fact the prologue is built against.
//!
//! The distance from a record to its function is the one number nothing in a compilation can work
//! out, since a function sits at a fixed offset inside a section a linker places. So it is left as
//! four zero bytes and a relocation, the same ordinary instruction pointer relative one an
//! instruction reaching a datum in the same file asks for, and the distance from the front of the
//! image that Windows wants instead.

use rucc_mir::CfiOp;
use rucc_object::{Extent, Marker, Reference, Reloc, Unwind};
use rucc_target::{CallRegs, ObjectFormat};

use crate::Error;

/// How many bytes an address in the table is worth, which is what a distance is divided by before
/// it is written.
///
/// One, because x86-64 instructions are not all the same length and there is no larger number every
/// distance is a multiple of. On a machine with fixed width instructions this is four and every
/// distance in the table is a quarter as long to write.
const CODE_ALIGN: u64 = 1;

/// How a record says where its function is: a distance from the four bytes themselves, signed.
///
/// `DW_EH_PE_pcrel | DW_EH_PE_sdata4`. A distance rather than an address because the table is read
/// in a process whose load address is not known when the file is written, and four bytes rather
/// than eight because no program puts two gigabytes between a function and its own unwind record.
const PCREL_SDATA4: u8 = 0x1b;

// The opcodes, which are DWARF's and are in section 6.4.2 of the standard. The three with the high
// bits set carry a small operand in the low six and have a longer form for the rest.
const NOP: u8 = 0x00;
const ADVANCE_LOC1: u8 = 0x02;
const ADVANCE_LOC2: u8 = 0x03;
const ADVANCE_LOC4: u8 = 0x04;
const OFFSET_EXTENDED: u8 = 0x05;
const RESTORE_EXTENDED: u8 = 0x06;
const REMEMBER_STATE: u8 = 0x0a;
const RESTORE_STATE: u8 = 0x0b;
const DEF_CFA: u8 = 0x0c;
const DEF_CFA_REGISTER: u8 = 0x0d;
const DEF_CFA_OFFSET: u8 = 0x0e;
const ADVANCE_LOC: u8 = 0x40;
const OFFSET: u8 = 0x80;
const RESTORE: u8 = 0xc0;

/// The largest register number the short forms of `DW_CFA_offset` and `DW_CFA_restore` can carry,
/// which is what the low six bits of a byte hold.
const SHORT_REG: u16 = 63;

/// Where each row of one function is and what it says.
///
/// The offset is from the start of the function rather than from the start of the section, because
/// a record counts from the function and because where the function itself lands is a number the
/// linker fills in.
pub(crate) type Rows = Vec<(usize, CfiOp)>;

/// The whole table for one object, in whichever of the two shapes the target reads.
///
/// Every function gets a record, including the ones with no rows in them. An unwinder that lands on
/// an address no record covers cannot tell a function that needed no rows from one that was never
/// described, and has to stop, which is exactly the failure this table exists to prevent. On
/// Windows it does worse than stop: a function with no row is a function it takes for a leaf, and
/// it walks off into whatever the frame happens to hold.
///
/// # Errors
///
/// [`Error::Frame`] for a prologue the target's table has no way to describe, which is only ever
/// Windows. See [`Error`].
pub(crate) fn table(
    funcs: &[Extent],
    rows: &[Rows],
    conv: &CallRegs,
    format: ObjectFormat,
) -> Result<Unwind, Error> {
    debug_assert_eq!(funcs.len(), rows.len(), "a record per function");
    if funcs.is_empty() {
        return Ok(Unwind::default());
    }
    match format {
        ObjectFormat::Elf => Ok(dwarf(funcs, rows, conv)),
        ObjectFormat::Coff => windows(funcs, rows, conv),
        // Nothing, because the other two answer the question their own way and neither is written
        // yet. Mach-O has a compact table of its own beside the DWARF one, and a WebAssembly module
        // is not a stack a table would describe. A table under a name their linker does not know is
        // a section nothing ever looks at, which is worse than none: it is the same bytes and the
        // same failure to unwind, with the size of the object spent on it.
        ObjectFormat::MachO | ObjectFormat::Wasm => Ok(Unwind::default()),
    }
}

/// The table the two formats that read DWARF want: one header, then one record per function.
fn dwarf(funcs: &[Extent], rows: &[Rows], conv: &CallRegs) -> Unwind {
    // Negative because every slot is below the end of the frame, and dividing by it is what makes
    // the number written for one positive, which is a byte shorter than a signed one.
    let align = usize::try_from(conv.word).expect("a pointer width").max(1);
    let mut table = Table { out: Unwind::default(), cie: 0, slot: -i64::from(conv.word), align };
    table.header(conv);
    for (func, rows) in funcs.iter().zip(rows) {
        table.record(func, rows);
    }
    table.out
}

/// The table being built, and the two facts about the target every record in it is written against.
struct Table {
    out: Unwind,
    /// Where the header is, which every record puts the distance back to.
    cie: usize,
    /// What a saved register's offset is divided by before it is written.
    slot: i64,
    /// What every record is padded out to, which is the pointer width.
    ///
    /// The length in front of a record already makes it possible to skip one without understanding
    /// it, so the padding is not what makes the table readable. It is what keeps the next record's
    /// fields aligned for a reader that takes the address of one instead of copying it out, and it
    /// is what gas does, so a table this compiler wrote and one an assembler wrote for the same
    /// instructions come out the same length.
    align: usize,
}

impl Table {
    /// The header every record in this object points back at.
    fn header(&mut self, conv: &CallRegs) {
        let start = self.out.bytes.len();
        self.cie = start;
        self.out.bytes.extend_from_slice(&0u32.to_le_bytes());
        // Zero is what says this is the header rather than a record. A record puts the distance
        // back to its header here, and a distance of zero would be a record pointing at itself.
        self.out.bytes.extend_from_slice(&0u32.to_le_bytes());
        self.out.bytes.push(1);
        // `z` says an augmentation section follows whose length is given, so a reader that does not
        // know the rest of the string can skip it. `R` says the augmentation holds how a record
        // spells the address of its function.
        self.out.bytes.extend_from_slice(b"zR\0");
        uleb(&mut self.out.bytes, CODE_ALIGN);
        sleb(&mut self.out.bytes, self.slot);
        uleb(&mut self.out.bytes, u64::from(conv.dwarf_return_address));
        uleb(&mut self.out.bytes, 1);
        self.out.bytes.push(PCREL_SDATA4);
        // The state a call leaves behind, which is where every function on this machine starts: the
        // frame ends one word above the stack pointer, because the call pushed a return address,
        // and that return address is the word below the end.
        let sp = conv
            .dwarf(conv.int_class, conv.stack_pointer)
            .expect("the stack pointer has a number in the table beside the register file");
        self.out.bytes.push(DEF_CFA);
        uleb(&mut self.out.bytes, u64::from(sp));
        uleb(&mut self.out.bytes, u64::from(conv.return_address));
        let below = -i32::try_from(conv.return_address).expect("a word");
        self.saved(conv.dwarf_return_address, below);
        self.pad(start);
    }

    /// One function's record.
    fn record(&mut self, func: &Extent, rows: &Rows) {
        let start = self.out.bytes.len();
        self.out.bytes.extend_from_slice(&0u32.to_le_bytes());
        // The distance back to the header, counted from this field rather than from the record,
        // which is how a reader that has just read the length knows where to look.
        let back = u32::try_from(self.out.bytes.len() - self.cie).expect("an object this size");
        self.out.bytes.extend_from_slice(&back.to_le_bytes());
        // Where the function is, which is four zero bytes and a relocation. The addend is zero
        // because what goes here is the distance from these bytes to the function's first
        // instruction, and that is what the relocation already means.
        self.out.relocs.push(Reloc {
            at: self.out.bytes.len(),
            symbol: func.name.clone(),
            kind: Reference::Data,
            addend: 0,
            // A table rather than an instruction, so the four bytes are the whole of what is here.
            after: 0,
        });
        self.out.bytes.extend_from_slice(&0u32.to_le_bytes());
        let len = u32::try_from(func.len).expect("a function this size");
        self.out.bytes.extend_from_slice(&len.to_le_bytes());
        // No augmentation of its own. The header said `zR` and `R` is answered there, so what is
        // left for a record is a length of zero, which still has to be written because `z`
        // promised a length would be there.
        uleb(&mut self.out.bytes, 0);
        let mut at = 0;
        for &(offset, op) in rows {
            self.advance(offset - at);
            at = offset;
            self.row(op);
        }
        self.pad(start);
    }

    /// One row, as the opcode DWARF spells it.
    fn row(&mut self, op: CfiOp) {
        match op {
            CfiOp::DefCfa { reg, offset } => {
                self.out.bytes.push(DEF_CFA);
                uleb(&mut self.out.bytes, u64::from(reg));
                uleb(&mut self.out.bytes, above(offset));
            }
            CfiOp::DefCfaOffset(offset) => {
                self.out.bytes.push(DEF_CFA_OFFSET);
                uleb(&mut self.out.bytes, above(offset));
            }
            CfiOp::DefCfaRegister(reg) => {
                self.out.bytes.push(DEF_CFA_REGISTER);
                uleb(&mut self.out.bytes, u64::from(reg));
            }
            CfiOp::Offset { reg, offset } => self.saved(reg, offset),
            CfiOp::Restore(reg) if reg <= SHORT_REG => {
                self.out.bytes.push(RESTORE | small(reg));
            }
            CfiOp::Restore(reg) => {
                self.out.bytes.push(RESTORE_EXTENDED);
                uleb(&mut self.out.bytes, u64::from(reg));
            }
            CfiOp::RememberState => self.out.bytes.push(REMEMBER_STATE),
            CfiOp::RestoreState => self.out.bytes.push(RESTORE_STATE),
        }
    }

    /// A register that went to a slot, at a distance below the end of the frame.
    ///
    /// The distance is divided by the slot size before it is written, which is what the header's
    /// data alignment is for and what makes most of these two bytes long. It comes out positive,
    /// because the alignment is negative and every slot is below the end of the frame.
    fn saved(&mut self, reg: u16, offset: i32) {
        let factored = i64::from(offset) / self.slot;
        debug_assert_eq!(
            factored * self.slot,
            i64::from(offset),
            "a slot is a whole number of slots below the end of the frame"
        );
        let factored = u64::try_from(factored).expect("a slot below the end of the frame");
        if reg <= SHORT_REG {
            self.out.bytes.push(OFFSET | small(reg));
        } else {
            self.out.bytes.push(OFFSET_EXTENDED);
            uleb(&mut self.out.bytes, u64::from(reg));
        }
        uleb(&mut self.out.bytes, factored);
    }

    /// How far along the function the next row takes effect, at the shortest of the four forms that
    /// holds it.
    fn advance(&mut self, by: usize) {
        let by = u64::try_from(by).expect("a function this size") / CODE_ALIGN;
        match by {
            0 => {}
            1..=0x3f => {
                let by = u8::try_from(by).expect("checked just above");
                self.out.bytes.push(ADVANCE_LOC | by);
            }
            0x40..=0xff => {
                self.out.bytes.push(ADVANCE_LOC1);
                self.out.bytes.push(u8::try_from(by).expect("checked just above"));
            }
            0x100..=0xffff => {
                self.out.bytes.push(ADVANCE_LOC2);
                let by = u16::try_from(by).expect("checked just above");
                self.out.bytes.extend_from_slice(&by.to_le_bytes());
            }
            _ => {
                self.out.bytes.push(ADVANCE_LOC4);
                let by = u32::try_from(by).expect("a function this size");
                self.out.bytes.extend_from_slice(&by.to_le_bytes());
            }
        }
    }

    /// Nops up to the alignment, and then the length of what was written into the four bytes in
    /// front of it.
    ///
    /// The length does not count itself, which is what lets a reader that does not understand a
    /// record skip it by reading four bytes and adding.
    fn pad(&mut self, start: usize) {
        while (self.out.bytes.len() - start) % self.align != 0 {
            self.out.bytes.push(NOP);
        }
        let len = u32::try_from(self.out.bytes.len() - start - 4).expect("a record this size");
        self.out.bytes[start..start + 4].copy_from_slice(&len.to_le_bytes());
    }
}

// The unwind codes Windows reads, which are in the low four bits of the second byte of a node. The
// four bits above them are the operand, which is a register for some of them and a size for others,
// and what does not fit there goes in the nodes that follow.
const PUSH_NONVOL: u8 = 0;
const ALLOC_LARGE: u8 = 1;
const ALLOC_SMALL: u8 = 2;
const SAVE_NONVOL: u8 = 4;
const SAVE_NONVOL_FAR: u8 = 5;
const SAVE_XMM128: u8 = 8;
const SAVE_XMM128_FAR: u8 = 9;

/// The largest frame `UWOP_ALLOC_SMALL` holds, which is the sixteen sizes its four bits count.
const SMALL_FRAME: i64 = 128;

/// What one instruction of a prologue did, once it is known which of the shapes this format has a
/// code for it was.
///
/// A step rather than a row, because the two do not line up: a push is two rows and one step, and
/// the rows that describe where the frame is counted from are not steps at all.
enum Step {
    /// A register went on the stack, named as DWARF numbers it.
    Push(u16),
    /// The frame was taken, that many bytes of it.
    Alloc(i64),
    /// A register went into a slot, named as DWARF numbers it, that far below the end of the frame.
    Save { reg: u16, from: i64 },
}

/// The table Windows wants: one row per function in one section, and the description each row
/// points at in another.
///
/// A description is reached by name rather than by distance, because the row and the description
/// are in two sections and there is no distance between two sections a compilation can work out.
/// The name is local, since what it points at is one function's prologue and no other object has
/// any use for it.
fn windows(funcs: &[Extent], rows: &[Rows], conv: &CallRegs) -> Result<Unwind, Error> {
    let mut out = Unwind::default();
    for (func, rows) in funcs.iter().zip(rows) {
        // The description first, because the row that points at it needs a name to point at and
        // the name is where the description landed.
        let name = format!("$unwind${}", func.name);
        let at = out.info.len();
        describe(&mut out.info, func, rows, conv)?;
        out.labels.push(Marker { name: name.clone(), at });
        // The row: where the function starts, one past where it ends, and where its description is.
        // None of the three is a number this compilation knows, since all of them are placed by the
        // linker, so each is four zero bytes and a relocation saying how far the thing is from the
        // front of the image. The second is the first plus the length, which the relocation carries
        // as its addend rather than as a second symbol at the end of the function.
        let len = i64::try_from(func.len).expect("a function this size");
        for (symbol, addend) in
            [(func.name.clone(), 0), (func.name.clone(), len), (name, 0)]
        {
            out.relocs.push(Reloc {
                at: out.bytes.len(),
                symbol,
                kind: Reference::Image,
                addend,
                // A table rather than an instruction, so the four bytes are the whole of what is
                // here and nothing of it comes after them.
                after: 0,
            });
            out.bytes.extend_from_slice(&0u32.to_le_bytes());
        }
    }
    Ok(out)
}

/// One function's prologue, as the record the runtime reads it from.
///
/// Four bytes of header and then the codes. The header is the version, which is one, and no flags,
/// since this compiler writes no exception handler and no record that continues another one; how
/// long the prologue is; how many nodes of codes follow; and which register the frame is counted
/// from, which is none, because the one prologue shape that would need one is refused below.
///
/// How long the prologue is is taken as where the last instruction that touched the frame ended,
/// rather than where the last instruction of the prologue ended. The two differ by the pieces that
/// describe nothing, which is the canary and the call to a profiler's hook, and what the number is
/// for is telling an address inside the prologue from one after it. An address in those trailing
/// pieces is one where the frame is already whole, so it is the right answer for both.
fn describe(info: &mut Vec<u8>, func: &Extent, rows: &Rows, conv: &CallRegs) -> Result<(), Error> {
    let codes = codes(func, rows, conv)?;
    let prologue = codes.last().map_or(0, |code| code[0]);
    let nodes = codes.iter().map(Vec::len).sum::<usize>() / 2;
    let count = u8::try_from(nodes).map_err(|_| {
        let why = format!("a prologue of {nodes} unwind slots, more than a record holds");
        frame(func, why)
    })?;
    info.extend_from_slice(&[1, prologue, count, 0]);
    // Backwards, because the runtime reads them from the address it is unwinding at and works its
    // way to the front of the function, so it wants the last thing the prologue did first.
    for code in codes.iter().rev() {
        info.extend_from_slice(code);
    }
    // Out to a whole number of four bytes. A record is read as words and the next one has to start
    // on one, and a header is four bytes already, so what is left to pad is an odd node count.
    if nodes % 2 != 0 {
        info.extend_from_slice(&[0, 0]);
    }
    Ok(())
}

/// The prologue's rows, as the codes that undo them, in the order the instructions ran.
///
/// Only the prologue: everything from the row that keeps the rules for the epilogues onwards is
/// about putting the frame back, and this format works that out by reading the instructions at the
/// address it is unwinding from rather than by being told.
///
/// Every code carries where the instruction that did it ended, which is what the rows already hold,
/// so the two are the same number and no translation is needed for it.
fn codes(func: &Extent, rows: &Rows, conv: &CallRegs) -> Result<Vec<Vec<u8>>, Error> {
    let end = rows.iter().position(|(_, op)| *op == CfiOp::RememberState).unwrap_or(rows.len());
    let rows = &rows[..end];
    let word = i64::from(conv.word);
    // How far the end of the frame is above the stack pointer, which starts at the return address
    // the call itself pushed and grows with everything the prologue puts below it.
    let mut below = i64::from(conv.return_address);
    let mut steps = Vec::new();
    let mut rest = rows;
    while let Some(&(at, _)) = rest.first() {
        // The rows one instruction produced, which is two for a push and one for everything else.
        let len = rest.iter().take_while(|(offset, _)| *offset == at).count();
        let (group, next) = rest.split_at(len);
        rest = next;
        let at = u8::try_from(at).map_err(|_| {
            frame(func, "a prologue longer than a record can count in a byte".to_owned())
        })?;
        match group {
            // A push, which says two things about one instruction: the end of the frame is a word
            // further up, and the register went in the word it just moved past. One code says both.
            [(_, CfiOp::DefCfaOffset(moved)), (_, CfiOp::Offset { reg, offset })]
                if i64::from(*moved) - below == word && i64::from(*offset) == -i64::from(*moved) =>
            {
                below += word;
                steps.push((at, Step::Push(*reg)));
            }
            [(_, CfiOp::DefCfaOffset(moved))] => {
                steps.push((at, Step::Alloc(i64::from(*moved) - below)));
                below = i64::from(*moved);
            }
            [(_, CfiOp::Offset { reg, offset })] => {
                steps.push((at, Step::Save { reg: *reg, from: i64::from(*offset) }));
            }
            // The frame pointer form. What cannot be said is not the pointer itself, which has a
            // code of its own, but what this compiler does after establishing it: the registers it
            // saves next sit below the frame the code would count from, and the frame it takes
            // afterwards has no row at all, since from there on the rules are counted from the
            // pointer and the stack pointer moving no longer changes them. A record without the
            // frame in it is a record that unwinds to the wrong place, so it is refused instead.
            [(_, CfiOp::DefCfaRegister(_))] => {
                let why = "a frame pointer established before the frame is taken";
                return Err(frame(func, why.to_owned()));
            }
            // The walk that touches every page of a large frame. While it runs, the end of the
            // frame is counted from a register holding where the walk stops, because the stack
            // pointer moves once an iteration and no fixed distance from it is true twice. This
            // format counts from the stack pointer and from a frame register and from nothing else.
            [(_, CfiOp::DefCfa { .. })] => {
                let why = "a stack walked a page at a time, whose frame is counted from a scratch \
                           register";
                return Err(frame(func, why.to_owned()));
            }
            _ => return Err(frame(func, "a prologue row this cannot read".to_owned())),
        }
    }
    // Every slot is measured from where the stack pointer ends the prologue, which is the one place
    // in the frame this format counts from, and the rows measure from the end of the frame instead.
    // The two are `below` apart once the prologue has done everything it does.
    steps.into_iter().map(|(at, step)| code(func, conv, at, step, below)).collect()
}

/// One step, as the nodes that say it.
fn code(
    func: &Extent,
    conv: &CallRegs,
    at: u8,
    step: Step,
    below: i64,
) -> Result<Vec<u8>, Error> {
    match step {
        Step::Push(reg) => Ok(vec![at, PUSH_NONVOL | machine(func, conv, reg)? << 4]),
        Step::Alloc(size)
            if (word_size(conv)..=SMALL_FRAME).contains(&size)
                && size % word_size(conv) == 0 =>
        {
            let steps = u8::try_from(size / word_size(conv) - 1).expect("a frame this small");
            Ok(vec![at, ALLOC_SMALL | steps << 4])
        }
        Step::Alloc(size) => large(func, at, size, word_size(conv)),
        Step::Save { reg, from } => slot(func, conv, at, reg, below + from),
    }
}

/// How big a slot is, which is what the two codes that count in slots divide by.
fn word_size(conv: &CallRegs) -> i64 {
    i64::from(conv.word).max(1)
}

/// A frame too big for the code that holds one in four bits, in the two forms that hold a larger
/// one.
///
/// The first counts in slots and fits a frame of half a megabyte in one extra node. The second
/// counts in bytes and takes two, which is every frame a program on this machine can have, since a
/// thread's stack is not four gigabytes.
fn large(func: &Extent, at: u8, size: i64, word: i64) -> Result<Vec<u8>, Error> {
    if size <= 0 || size % word != 0 {
        let why = format!("a frame of {size} bytes, not a whole number of slots");
        return Err(frame(func, why));
    }
    let mut out = vec![at, ALLOC_LARGE];
    if let Ok(slots) = u16::try_from(size / word) {
        out.extend_from_slice(&slots.to_le_bytes());
        return Ok(out);
    }
    let bytes = u32::try_from(size).map_err(|_| {
        frame(func, format!("a frame of {size} bytes, larger than a record can say"))
    })?;
    out[1] |= 1 << 4;
    out.extend_from_slice(&bytes.to_le_bytes());
    Ok(out)
}

/// A register that went into a slot, at a distance above where the stack pointer ends the prologue.
///
/// Two codes per register class and the same choice between them: one counts in slots and holds
/// what fits in a node, and one counts in bytes and takes two nodes for anything else. A general
/// purpose register counts in words and a vector register counts in sixteens, which is what one of
/// them is.
fn slot(
    func: &Extent,
    conv: &CallRegs,
    at: u8,
    reg: u16,
    above: i64,
) -> Result<Vec<u8>, Error> {
    if above < 0 {
        let why = format!("a register saved {} bytes below its own frame", -above);
        return Err(frame(func, why));
    }
    let bytes = u32::try_from(above).map_err(|_| {
        frame(func, format!("a register saved {above} bytes up, further than a record reaches"))
    })?;
    let vector = conv.machine(conv.int_class, reg).is_none();
    let (near, far, step) = if vector {
        (SAVE_XMM128, SAVE_XMM128_FAR, 16)
    } else {
        (SAVE_NONVOL, SAVE_NONVOL_FAR, u32::try_from(word_size(conv)).expect("a pointer width"))
    };
    let number = machine(func, conv, reg)?;
    let scaled = (above % i64::from(step) == 0).then(|| u16::try_from(bytes / step).ok()).flatten();
    let mut out = vec![at, if scaled.is_some() { near } else { far } | number << 4];
    match scaled {
        Some(scaled) => out.extend_from_slice(&scaled.to_le_bytes()),
        None => out.extend_from_slice(&bytes.to_le_bytes()),
    }
    Ok(out)
}

/// Which register of the machine one the rows name is, as the number this table is written in.
///
/// The rows carry DWARF's numbering, because that is what the format two of the three platforms
/// read is written in, and the codes here carry the machine's own. The two disagree over four of
/// the general purpose registers on this machine and nowhere else, which is the worst shape a
/// disagreement can have: every number is a register either way, so a table written in the wrong
/// one comes out well formed and about the wrong registers.
fn machine(func: &Extent, conv: &CallRegs, reg: u16) -> Result<u8, Error> {
    let found = conv
        .machine(conv.int_class, reg)
        .or_else(|| conv.machine(conv.sse_class, reg))
        .map(|reg| reg.number())
        .filter(|number| *number < 16);
    found.ok_or_else(|| {
        frame(func, format!("a register saved under DWARF number {reg}, which this machine has none of"))
    })
}

/// A prologue this cannot describe, named by the function it is the prologue of.
fn frame(func: &Extent, why: String) -> Error {
    Error::Frame { func: func.name.clone(), why }
}

/// A register number small enough to ride in the low six bits of an opcode.
fn small(reg: u16) -> u8 {
    u8::try_from(reg).expect("a register number the caller checked")
}

/// An offset from the end of the frame, written unsigned because it is always positive: the end of
/// a frame is above the stack pointer and never below it.
fn above(offset: i32) -> u64 {
    u64::try_from(offset).expect("a frame that ends above the stack pointer")
}

/// One number, seven bits at a time, low bits first, with the high bit set on every byte but the
/// last.
fn uleb(bytes: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = u8::try_from(value & 0x7f).expect("seven bits");
        value >>= 7;
        if value == 0 {
            bytes.push(byte);
            return;
        }
        bytes.push(byte | 0x80);
    }
}

/// The same, signed, where the last byte's sixth bit is the sign and the value is sign extended out
/// of it rather than zero extended.
fn sleb(bytes: &mut Vec<u8>, mut value: i64) {
    loop {
        let byte = u8::try_from(value & 0x7f).expect("seven bits");
        value >>= 7;
        let done = (value == 0 && byte & 0x40 == 0) || (value == -1 && byte & 0x40 != 0);
        if done {
            bytes.push(byte);
            return;
        }
        bytes.push(byte | 0x80);
    }
}

#[cfg(test)]
mod tests {
    use rucc_object::{Binding, Visibility};
    use rucc_target::x86_64::WIN64;

    use super::*;

    /// DWARF's number for the four registers a prologue here pushes, which is not the machine's for
    /// any of them but the third.
    const RBP: u16 = 6;
    const RBX: u16 = 3;
    const R12: u16 = 12;
    const XMM6: u16 = 23;

    /// A function of that name and length, with nothing about it that matters here.
    fn func(name: &str, len: usize) -> Extent {
        Extent {
            name: name.to_owned(),
            start: 0,
            len,
            align: 16,
            binding: Binding::Global,
            visibility: Visibility::Default,
            patch: None,
        }
    }

    /// The description one function's rows come out as, with the header and the padding.
    fn info(rows: Rows) -> Vec<u8> {
        let out = table(&[func("f", 64)], &[rows], &WIN64, ObjectFormat::Coff)
            .expect("a prologue this can describe");
        out.info
    }

    /// Why a prologue was refused, for a prologue that was.
    fn refused(rows: Rows) -> String {
        let out = table(&[func("f", 64)], &[rows], &WIN64, ObjectFormat::Coff)
            .expect_err("a prologue this cannot describe");
        out.to_string()
    }

    /// The ordinary shape: the callee saved registers go on the stack and then the frame is taken
    /// in one subtraction, which is three codes and the last of them first.
    ///
    /// The register numbers are the machine's rather than DWARF's, which is the one translation
    /// this table needs and the one a wrong table would still look well formed without.
    #[test]
    fn a_prologue_of_pushes_and_a_frame_is_the_codes_that_undo_it() {
        let rows = vec![
            (1, CfiOp::DefCfaOffset(16)),
            (1, CfiOp::Offset { reg: RBP, offset: -16 }),
            (2, CfiOp::DefCfaOffset(24)),
            (2, CfiOp::Offset { reg: RBX, offset: -24 }),
            (8, CfiOp::DefCfaOffset(56)),
            (8, CfiOp::RememberState),
        ];
        assert_eq!(
            info(rows),
            vec![
                // Version one and no flags, a prologue of eight bytes, three nodes, and no
                // register the frame is counted from.
                1, 8, 3, 0, //
                // Thirty two bytes of frame, which is four slots and is written as one less.
                8, ALLOC_SMALL | (3 << 4), //
                // rbx, which is three to DWARF and three to the machine.
                2, PUSH_NONVOL | (3 << 4), //
                // rbp, which is six to DWARF and five to the machine.
                1, PUSH_NONVOL | (5 << 4), //
                // An odd number of nodes, padded so the next record starts on a word.
                0, 0,
            ]
        );
    }

    /// A frame larger than the four bits of a code, in the form that counts slots, and one larger
    /// than that form holds, in the form that counts bytes.
    #[test]
    fn a_frame_too_big_for_a_code_goes_in_the_nodes_behind_it() {
        let one = vec![(4, CfiOp::DefCfaOffset(8 + 4096)), (4, CfiOp::RememberState)];
        assert_eq!(info(one), vec![1, 4, 2, 0, 4, ALLOC_LARGE, 0x00, 0x02]);

        let huge = vec![(7, CfiOp::DefCfaOffset(8 + 8 * 0x1_0000)), (7, CfiOp::RememberState)];
        let want = vec![1, 7, 3, 0, 7, ALLOC_LARGE | (1 << 4), 0x00, 0x00, 0x08, 0x00, 0, 0];
        assert_eq!(info(huge), want);
    }

    /// A register that went into a slot rather than onto the stack, which is what a vector register
    /// does here and what the two codes that carry an offset are for.
    ///
    /// The offset is from where the stack pointer ends the prologue, which is the one place this
    /// format counts from, and the rows count from the end of the frame instead.
    #[test]
    fn a_register_saved_in_a_slot_is_measured_from_the_end_of_the_prologue() {
        let rows = vec![
            (1, CfiOp::DefCfaOffset(16)),
            (1, CfiOp::Offset { reg: RBP, offset: -16 }),
            (8, CfiOp::DefCfaOffset(56)),
            (14, CfiOp::Offset { reg: XMM6, offset: -40 }),
            (14, CfiOp::RememberState),
        ];
        assert_eq!(
            info(rows),
            vec![
                1, 14, 4, 0, //
                // xmm6 at sixteen bytes up, which the code counts in sixteens.
                14, SAVE_XMM128 | (6 << 4), 0x01, 0x00, //
                8, ALLOC_SMALL | (4 << 4), //
                1, PUSH_NONVOL | (5 << 4),
            ]
        );
    }

    /// A general purpose register in a slot, in the same shape, counted in words instead.
    #[test]
    fn a_general_purpose_register_in_a_slot_counts_in_words() {
        let rows = vec![
            (8, CfiOp::DefCfaOffset(72)),
            (13, CfiOp::Offset { reg: R12, offset: -48 }),
            (13, CfiOp::RememberState),
        ];
        assert_eq!(
            info(rows),
            vec![
                1, 13, 3, 0, //
                // r12, which is twelve to both, three words up.
                13, SAVE_NONVOL | (12 << 4), 0x03, 0x00, //
                8, ALLOC_SMALL | (7 << 4), //
                0, 0,
            ]
        );
    }

    /// A function with nothing to say still gets a description, because a function with no row at
    /// all is a function this platform takes for a leaf and walks straight through.
    #[test]
    fn a_leaf_gets_an_empty_description_rather_than_none() {
        assert_eq!(info(Vec::new()), vec![1, 0, 0, 0]);
    }

    /// The row: where the function starts, one past where it ends, and where its description is.
    /// None of the three is a number this compilation knows, so each is four zero bytes and a
    /// relocation, and the one that points at the description points at a name of its own.
    #[test]
    fn every_function_gets_a_row_of_three_places_the_linker_fills_in() {
        let funcs = [func("one", 32), func("two", 48)];
        let out = table(&funcs, &[Vec::new(), Vec::new()], &WIN64, ObjectFormat::Coff)
            .expect("two leaves");
        assert_eq!(out.bytes, vec![0; 24], "three empty fields per function");
        let places: Vec<_> =
            out.relocs.iter().map(|reloc| (reloc.symbol.as_str(), reloc.addend)).collect();
        assert_eq!(
            places,
            vec![
                ("one", 0),
                ("one", 32),
                ("$unwind$one", 0),
                ("two", 0),
                ("two", 48),
                ("$unwind$two", 0),
            ]
        );
        assert!(out.relocs.iter().all(|reloc| reloc.kind == Reference::Image));
        let labels: Vec<_> = out.labels.iter().map(|label| (label.name.as_str(), label.at)).collect();
        assert_eq!(labels, vec![("$unwind$one", 0), ("$unwind$two", 4)]);
    }

    /// The frame pointer form, which is refused rather than described. What cannot be said is not
    /// the pointer but the frame taken after it, which has no row and would come out as a record
    /// that unwinds to the wrong place.
    #[test]
    fn a_prologue_this_cannot_describe_is_refused_by_name() {
        let pointer = vec![
            (1, CfiOp::DefCfaOffset(16)),
            (1, CfiOp::Offset { reg: RBP, offset: -16 }),
            (4, CfiOp::DefCfaRegister(RBP)),
            (4, CfiOp::RememberState),
        ];
        let why = refused(pointer);
        assert!(why.contains("'f'"), "{why}");
        assert!(why.contains("frame pointer"), "{why}");

        let walked = vec![(9, CfiOp::DefCfa { reg: 0, offset: 65544 }), (9, CfiOp::RememberState)];
        let why = refused(walked);
        assert!(why.contains("a page at a time"), "{why}");

        let long = vec![(300, CfiOp::DefCfaOffset(16)), (300, CfiOp::RememberState)];
        assert!(refused(long).contains("longer than"), "a prologue no record can count in a byte");
    }

    /// The other formats get nothing rather than a table under a name their linker has never heard
    /// of, which would be the same bytes and the same failure to unwind with the size spent on it.
    #[test]
    fn a_format_whose_table_is_not_written_yet_gets_no_section() {
        let rows = vec![vec![(1, CfiOp::DefCfaOffset(16))]];
        let mach = table(&[func("f", 8)], &rows, &WIN64, ObjectFormat::MachO).expect("nothing");
        assert_eq!(mach, Unwind::default());
    }
}
