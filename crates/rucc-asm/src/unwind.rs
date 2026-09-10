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
//! # What is here and what is not
//!
//! Here: the encoding, which is DWARF's and is the same on every format that carries this table.
//! Not here: where the section goes and what its flags are, which is the object writer's, and which
//! rows a function has, which is the code generator's because the prologue is the only thing that
//! knows what it did. What the header says is read out of the calling convention rather than
//! written down again, since the state a call leaves behind is the same fact the prologue is built
//! against.
//!
//! The distance from a record to its function is the one number nothing in a compilation can work
//! out, since a function sits at a fixed offset inside a section a linker places. So it is left as
//! four zero bytes and a relocation, the same ordinary instruction pointer relative one an
//! instruction reaching a datum in the same file asks for.

use rucc_mir::CfiOp;
use rucc_object::{Extent, Reference, Reloc, Unwind};
use rucc_target::CallRegs;

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

/// The whole table for one object: one header, then one record per function.
///
/// Every function gets a record, including the ones with no rows in them. An unwinder that lands on
/// an address no record covers cannot tell a function that needed no rows from one that was never
/// described, and has to stop, which is exactly the failure this table exists to prevent.
pub(crate) fn table(funcs: &[Extent], rows: &[Rows], conv: &CallRegs) -> Unwind {
    debug_assert_eq!(funcs.len(), rows.len(), "a record per function");
    if funcs.is_empty() {
        return Unwind::default();
    }
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
