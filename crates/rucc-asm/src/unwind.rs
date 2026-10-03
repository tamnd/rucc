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
use rucc_object::{Chunk, EXCEPT_TABLE, Extent, Marker, Reference, Reloc, Unwind};
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

/// The same, read through: the four bytes are the distance to a pointer and the pointer is the
/// answer, which is how the header reaches the personality routine.
const INDIRECT_PCREL_SDATA4: u8 = 0x9b;

/// A field of the call site table that is not there, and a pointer a directive says is not there.
pub(crate) const OMIT: u8 = 0xff;

/// The bit of a pointer's encoding that says the pointer is to a pointer that holds the answer.
const INDIRECT: u8 = 0x80;

/// How the rows of a call site table spell their numbers, which is as unsigned LEB128.
const ULEB128: u8 = 0x01;

// The opcodes, which are DWARF's and are in section 6.4.2 of the standard. The three with the high
// bits set carry a small operand in the low six and have a longer form for the rest.
const NOP: u8 = 0x00;
const ADVANCE_LOC1: u8 = 0x02;
const ADVANCE_LOC2: u8 = 0x03;
const ADVANCE_LOC4: u8 = 0x04;
const OFFSET_EXTENDED: u8 = 0x05;
const OFFSET_EXTENDED_SF: u8 = 0x11;
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

/// A personality routine and a call site table that a file of assembly named for one function,
/// with `.cfi_personality` and `.cfi_lsda`, rather than ones this crate works out from the landing
/// pads of a function it compiled.
///
/// This is how a listing says the same thing [`Extent::landings`] says, and it is what gcc writes
/// for a function with a `cleanup` under `-fexceptions`. The call site table is then bytes the
/// file wrote into its own section, so all a record needs is the name of where it starts, and the
/// header needs the name of the routine. Each comes with the encoding the directive gave it, which
/// is written into the header so the unwinder reads the pointer the way it was written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Named {
    /// The routine's encoding and name.
    pub(crate) personality: (u8, String),
    /// Where the call site table is, when the function has one.
    pub(crate) lsda: Option<(u8, String)>,
}

/// How a file of assembly began one function's rules, with `.cfi_startproc` and
/// `.cfi_signal_frame`, which is the same for every function the compiler writes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Begins {
    /// `.cfi_startproc simple`, which leaves out the state a call leaves behind, for a function
    /// nothing called, like the code the kernel returns to from a signal handler.
    pub(crate) simple: bool,
    /// `.cfi_signal_frame`, which tells the unwinder the frame was not made by a call, so the
    /// address it finds is the instruction to resume at rather than the one after a call.
    pub(crate) signal: bool,
}

/// How many bytes a pointer in an encoding the table can write takes, which is `None` for one it
/// cannot.
///
/// The low four bits are the size and the next three say what the pointer is counted from. Two of
/// those can be written with the relocations every object already has: nothing, which is an
/// address, and the pointer's own place, which is a distance. An address is four or eight bytes,
/// or a whole pointer for `absptr`. A distance is four bytes, since a relocation for eight bytes
/// of distance is not one the object writer has. The top bit, a pointer to the pointer, changes
/// what the unwinder does with the answer and not how it is written, so it is allowed for the
/// routine, which is the one gcc spells that way.
pub(crate) fn pointer_size(encoding: u8, word: u8, indirect: bool) -> Option<u8> {
    if encoding & INDIRECT != 0 && !indirect {
        return None;
    }
    match (encoding & 0x70, encoding & 0x0f) {
        (0x00, 0x00) => Some(word),
        (0x00, 0x03 | 0x0b) | (0x10, 0x03 | 0x0b) => Some(4),
        (0x00, 0x04 | 0x0c) => Some(8),
        _ => None,
    }
}

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
///
/// `named` is what a file of assembly said about each function's personality routine, one entry
/// per function or none at all, and is only read on ELF. See [`Named`]. `begins` is the same for
/// how each function's rules began, and none at all is the way a call leaves a frame for every one.
/// See [`Begins`].
pub(crate) fn table(
    funcs: &[Extent],
    rows: &[Rows],
    conv: &CallRegs,
    format: ObjectFormat,
    named: &[Option<Named>],
    begins: &[Begins],
) -> Result<Unwind, Error> {
    debug_assert_eq!(funcs.len(), rows.len(), "a record per function");
    if funcs.is_empty() {
        return Ok(Unwind::default());
    }
    match format {
        ObjectFormat::Elf => Ok(dwarf(funcs, rows, conv, named, begins)),
        ObjectFormat::Coff => windows(funcs, rows, conv),
        // The same DWARF records, in `__TEXT,__eh_frame`, which ld64 reads and turns into the
        // compact table the unwinder searches, one entry per function that points back at its
        // record. That compact table can also be written by the compiler, and a function whose
        // prologue fits one of its shapes then needs no record at all, but that only makes the
        // image smaller and is left for later. The record's pointers are the same distances they
        // are on ELF, and the object writer says each as the pair of relocations Mach-O spells a
        // distance with. See `rucc_object::macho::reloc`.
        //
        // Without a personality routine or a call site table for now: the routine's pointer is a
        // slot gcc's runtime fills in on ELF and the table's section is ELF's, and Apple has its
        // own spelling of both. So a function with a landing pad is unwound through and its
        // cleanups are not run, which is the same as what a program built without `-fexceptions`
        // does and better than an unwinder that stops at the first frame of ours it meets.
        ObjectFormat::MachO => {
            let bare: Vec<Extent> =
                funcs.iter().map(|func| Extent { landings: Vec::new(), ..func.clone() }).collect();
            Ok(dwarf(&bare, rows, conv, &[], begins))
        }
        // Nothing, because a WebAssembly module is not a stack a table would describe. A table
        // under a name its linker does not know is a section nothing ever looks at, which is worse
        // than none: it is the same bytes and the same failure to unwind, with the size of the
        // object spent on it.
        ObjectFormat::Wasm => Ok(Unwind::default()),
    }
}

/// The same rows as a debugger's copy of the table, which is `.debug_frame` rather than
/// `.eh_frame`, for a build that asked for debug information and for no unwind table.
///
/// A debugger reads a frame base through a table like this one, and without one it has no way to
/// say where a local on the stack is. The unwind table would answer it, but it is loaded with the
/// program, and a build that turned it off asked for that not to happen. This is what gcc writes
/// in the same case: the same rules, in a section the loader never maps, so the program costs the
/// same as one with no table at all.
///
/// Nothing on a format other than ELF, for the reason [`table`] gives nothing there.
pub(crate) fn debug_frame(
    funcs: &[Extent],
    rows: &[Rows],
    conv: &CallRegs,
    format: ObjectFormat,
    begins: &[Begins],
) -> Option<Chunk> {
    debug_assert_eq!(funcs.len(), rows.len(), "a record per function");
    if funcs.is_empty() || format != ObjectFormat::Elf {
        return None;
    }
    let mut table = Table::new(conv, true);
    for (at, (func, rows)) in funcs.iter().zip(rows).enumerate() {
        let begins = begins.get(at).copied().unwrap_or_default();
        let (first, rest) = leading(conv, begins, rows);
        table.choose(conv, Kind::Plain, None, begins.signal, first);
        table.record(func, rest);
    }
    Some(Chunk { name: DEBUG_FRAME.to_owned(), bytes: table.out.bytes, relocs: table.out.relocs })
}

/// What the debugger's copy of the table is called, which is also what its records name to say
/// where their header is.
const DEBUG_FRAME: &str = ".debug_frame";

/// The table the two formats that read DWARF want: a header, then one record per function that
/// points back at it.
///
/// A header names the personality routine when a function has a landing pad, and each function
/// with one points at that header instead and says where its call site table is. A function a file
/// of assembly named a routine for gets a header that names that one. See [`Named`].
///
/// A header also holds the rules a function starts with, which are the state a call leaves behind
/// and then whatever the function says before its first instruction, so two functions share one
/// only when those are the same too. This is how gas picks them, and each is written just in front
/// of the first record that needs it, which is also where gas puts it. See [`leading`].
fn dwarf(
    funcs: &[Extent],
    rows: &[Rows],
    conv: &CallRegs,
    named: &[Option<Named>],
    begins: &[Begins],
) -> Unwind {
    let mut table = Table::new(conv, false);
    for (at, (func, rows)) in funcs.iter().zip(rows).enumerate() {
        let said = named.get(at).and_then(Option::as_ref);
        let begins = begins.get(at).copied().unwrap_or_default();
        let (first, rest) = leading(conv, begins, rows);
        match (func.landings.is_empty(), said) {
            (false, _) => {
                table.choose(conv, Kind::Personal, None, begins.signal, first);
                let lsda = table.call_sites(func);
                table.lsda = Some(Lsda::At(lsda));
            }
            (true, Some(said)) => {
                let (encoding, routine) = &said.personality;
                let key = (*encoding, routine.clone(), said.lsda.as_ref().map(|(lsda, _)| *lsda));
                table.choose(conv, Kind::Named(key), Some(said), begins.signal, first);
                table.lsda = said.lsda.clone().map(|(encoding, name)| Lsda::Named(encoding, name));
            }
            (true, None) => {
                table.choose(conv, Kind::Plain, None, begins.signal, first);
                table.lsda = None;
            }
        }
        table.record(func, rest);
    }
    table.out
}

/// The rules a function's header holds rather than its record, and the rows left for the record.
///
/// The state a call leaves behind, unless the file said `simple`, and then every row that comes
/// before the function's first instruction, up to one that puts the state away, since the
/// unwinder has to read that one in the record to know where to come back to. On x86-64 the frame
/// ends one word above the stack pointer, because the call pushed a return address, and that
/// return address is the word below the end. On AArch64 the call pushed nothing and left the
/// return address in a register, so the frame ends at the stack pointer and there is no slot to
/// say anything about.
fn leading<'a>(
    conv: &CallRegs,
    begins: Begins,
    rows: &'a [(usize, CfiOp)],
) -> (Vec<CfiOp>, &'a [(usize, CfiOp)]) {
    let mut first = Vec::new();
    if !begins.simple {
        let reg = conv
            .dwarf(conv.int_class, conv.stack_pointer)
            .expect("the stack pointer has a number in the table beside the register file");
        let word = i32::try_from(conv.return_address).expect("a word");
        first.push(CfiOp::DefCfa { reg, offset: word });
        if word != 0 {
            first.push(CfiOp::Offset { reg: conv.dwarf_return_address, offset: -word });
        }
    }
    let ahead = rows.iter().take_while(|&&(at, op)| at == 0 && op != CfiOp::RememberState).count();
    first.extend(rows[..ahead].iter().map(|&(_, op)| op));
    (first, &rows[ahead..])
}

/// What a header says about the routine of the functions that point at it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    /// Nothing, for a function with no landing pad.
    Plain,
    /// The routine C uses, for a function this crate compiled with a landing pad.
    Personal,
    /// The routine a file of assembly named, in the encodings it named it in.
    Named(HeaderKey),
}

/// What makes two headers for named routines the same one. See [`dwarf`].
type HeaderKey = (u8, String, Option<u8>);

/// What makes two headers the same one: the routine, whether the frame is a signal's, and the
/// rules every function pointing at it starts with.
type Header = (Kind, bool, Vec<CfiOp>);

/// Where a record's call site table is.
enum Lsda {
    /// At this offset in [`EXCEPT_TABLE`], which this crate wrote.
    At(usize),
    /// At a name a file of assembly gave it, spelled in this encoding.
    Named(u8, String),
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
    /// Whether this is the debugger's copy, which spells three things differently: what marks the
    /// header, how a record says where its header is, and how it says where its function is.
    debug: bool,
    /// Where the call site table of the record being written is, when it has one. See
    /// [`Table::call_sites`] and [`Named`].
    lsda: Option<Lsda>,
    /// Every header written so far, by what it says, and where it is.
    headers: Vec<(Header, usize)>,
}

impl Table {
    /// An empty table for the target, in one shape or the other.
    fn new(conv: &CallRegs, debug: bool) -> Self {
        let align = usize::try_from(conv.word).expect("a pointer width").max(1);
        // Negative because every slot is below the end of the frame, and dividing by it is what
        // makes the number written for one positive, which is a byte shorter than a signed one.
        let slot = -i64::from(conv.word);
        Self { out: Unwind::default(), cie: 0, slot, align, debug, lsda: None, headers: Vec::new() }
    }

    /// Points the next record at a header that says what it needs, writing one when none written so
    /// far does. See [`dwarf`].
    fn choose(
        &mut self,
        conv: &CallRegs,
        kind: Kind,
        said: Option<&Named>,
        signal: bool,
        first: Vec<CfiOp>,
    ) {
        let key = (kind, signal, first);
        if let Some(&(_, cie)) = self.headers.iter().find(|(seen, _)| *seen == key) {
            self.cie = cie;
            return;
        }
        let (kind, signal, first) = &key;
        match (kind, said) {
            (Kind::Personal, _) => self.personal_header(conv, *signal, first),
            (Kind::Named(_), Some(said)) => self.named_header(conv, said, *signal, first),
            _ => self.header(conv, *signal, first),
        }
        self.headers.push((key, self.cie));
    }

    /// The header the records of functions with no routine point back at.
    fn header(&mut self, conv: &CallRegs, signal: bool, first: &[CfiOp]) {
        let start = self.out.bytes.len();
        self.cie = start;
        self.out.bytes.extend_from_slice(&0u32.to_le_bytes());
        // What says this is the header rather than a record. Zero in the unwind table, where a
        // record puts the distance back to its header here and a distance of zero would be a
        // record pointing at itself. All ones in the debugger's copy, where a record puts the
        // header's offset from the front of the section here and zero is an offset one can be at.
        let id = if self.debug { u32::MAX } else { 0 };
        self.out.bytes.extend_from_slice(&id.to_le_bytes());
        self.out.bytes.push(1);
        // `z` says an augmentation section follows whose length is given, so a reader that does not
        // know the rest of the string can skip it. `R` says the augmentation holds how a record
        // spells the address of its function. The debugger's copy has none, since a record there
        // spells it the one way DWARF has, as an address the width of a pointer.
        // `S` says the frame was made by a signal and not by a call. See [`Begins`].
        let augmentation: &[u8] = match (self.debug, signal) {
            (true, false) => b"\0",
            (true, true) => b"S\0",
            (false, false) => b"zR\0",
            (false, true) => b"zRS\0",
        };
        self.out.bytes.extend_from_slice(augmentation);
        uleb(&mut self.out.bytes, CODE_ALIGN);
        sleb(&mut self.out.bytes, self.slot);
        uleb(&mut self.out.bytes, u64::from(conv.dwarf_return_address));
        if !self.debug {
            uleb(&mut self.out.bytes, 1);
            self.out.bytes.push(PCREL_SDATA4);
        }
        self.starts(start, first);
    }

    /// The header the records of functions with a landing pad point back at.
    ///
    /// `zPLR` rather than `zR`. `P` is the personality routine, which the unwinder calls for every
    /// frame of this kind it passes through and which is what reads the call site table and sends
    /// control to a pad. It is reached through the pointer the lowering made, because the routine
    /// is in a shared library and this section is not written at load time. `L` says each record
    /// has the distance to its call site table in its augmentation. Both are gcc's encodings for
    /// position independent code, which is what everything here is.
    fn personal_header(&mut self, conv: &CallRegs, signal: bool, first: &[CfiOp]) {
        let start = self.out.bytes.len();
        self.cie = start;
        self.out.bytes.extend_from_slice(&0u32.to_le_bytes());
        self.out.bytes.extend_from_slice(&0u32.to_le_bytes());
        self.out.bytes.push(1);
        let augmentation: &[u8] = if signal { b"zPLRS\0" } else { b"zPLR\0" };
        self.out.bytes.extend_from_slice(augmentation);
        uleb(&mut self.out.bytes, CODE_ALIGN);
        sleb(&mut self.out.bytes, self.slot);
        uleb(&mut self.out.bytes, u64::from(conv.dwarf_return_address));
        // The encoding byte and four bytes of pointer for `P`, and a byte each for `L` and `R`.
        uleb(&mut self.out.bytes, 7);
        self.out.bytes.push(INDIRECT_PCREL_SDATA4);
        self.out.relocs.push(Reloc {
            at: self.out.bytes.len(),
            symbol: rucc_ir::PERSONALITY_REF.to_owned(),
            kind: Reference::Data,
            addend: 0,
            after: 0,
        });
        self.out.bytes.extend_from_slice(&0u32.to_le_bytes());
        self.out.bytes.push(PCREL_SDATA4);
        self.out.bytes.push(PCREL_SDATA4);
        self.starts(start, first);
    }

    /// The header the records of functions a file of assembly named a personality routine for
    /// point back at.
    ///
    /// The same shape as [`Table::personal_header`], with the routine and the encodings the file
    /// gave. `L` is left out when the file named no call site table, and then a record has no
    /// pointer to one either, which is what gas writes for `.cfi_personality` on its own.
    fn named_header(&mut self, conv: &CallRegs, said: &Named, signal: bool, first: &[CfiOp]) {
        let start = self.out.bytes.len();
        self.cie = start;
        let word = u8::try_from(self.align).expect("a pointer width");
        self.out.bytes.extend_from_slice(&0u32.to_le_bytes());
        self.out.bytes.extend_from_slice(&0u32.to_le_bytes());
        self.out.bytes.push(1);
        let augmentation: &[u8] = match (said.lsda.is_some(), signal) {
            (true, false) => b"zPLR\0",
            (true, true) => b"zPLRS\0",
            (false, false) => b"zPR\0",
            (false, true) => b"zPRS\0",
        };
        self.out.bytes.extend_from_slice(augmentation);
        uleb(&mut self.out.bytes, CODE_ALIGN);
        sleb(&mut self.out.bytes, self.slot);
        uleb(&mut self.out.bytes, u64::from(conv.dwarf_return_address));
        let (encoding, routine) = &said.personality;
        let size = pointer_size(*encoding, word, true).expect("an encoding the reader checked");
        let lsda = u64::from(said.lsda.is_some());
        uleb(&mut self.out.bytes, 1 + u64::from(size) + lsda + 1);
        self.out.bytes.push(*encoding);
        self.pointer(*encoding, routine, size);
        if let Some((encoding, _)) = &said.lsda {
            self.out.bytes.push(*encoding);
        }
        self.out.bytes.push(PCREL_SDATA4);
        self.starts(start, first);
    }

    /// A pointer to `symbol` in `size` bytes of `encoding`, as zeroes and the relocation that
    /// fills them in. See [`pointer_size`].
    fn pointer(&mut self, encoding: u8, symbol: &str, size: u8) {
        let kind = if encoding & 0x70 == 0x10 {
            Reference::Data
        } else {
            Reference::Address { bytes: size }
        };
        let at = self.out.bytes.len();
        self.out.relocs.push(Reloc { at, symbol: symbol.to_owned(), kind, addend: 0, after: 0 });
        self.out.bytes.resize(at + usize::from(size), 0);
    }

    /// Where the call site table of one function goes, written at the end of [`EXCEPT_TABLE`], and
    /// the offset it starts at.
    ///
    /// gcc's layout for a C function. No landing pad base, so the pads are counted from the start
    /// of the function, and no type table, since nothing in C is caught by type. Then the rows,
    /// one per call with a pad, each the call's offset and length and the pad's offset as unsigned
    /// numbers of as many bytes as they need, and an action of zero, which says the pad is a
    /// cleanup and the unwind goes on once it has run. A call with no row is one the unwind passes
    /// through, which is what the C personality does with an address the table does not cover.
    fn call_sites(&mut self, func: &Extent) -> usize {
        let out = &mut self.out.except;
        while out.len() % 4 != 0 {
            out.push(0);
        }
        let at = out.len();
        out.push(OMIT);
        out.push(OMIT);
        out.push(ULEB128);
        let mut rows = Vec::new();
        for site in &func.landings {
            uleb(&mut rows, site.start as u64);
            uleb(&mut rows, site.len as u64);
            uleb(&mut rows, site.pad as u64);
            uleb(&mut rows, 0);
        }
        uleb(out, rows.len() as u64);
        out.extend_from_slice(&rows);
        at
    }

    /// What every header ends with: the rules every function that points at it starts with, and
    /// the padding. See [`leading`].
    fn starts(&mut self, start: usize, first: &[CfiOp]) {
        for &op in first {
            self.row(op);
        }
        self.pad(start);
    }

    /// One function's record.
    fn record(&mut self, func: &Extent, rows: &[(usize, CfiOp)]) {
        let start = self.out.bytes.len();
        self.out.bytes.extend_from_slice(&0u32.to_le_bytes());
        if self.debug {
            self.debug_record(func);
        } else {
            self.eh_record(func);
        }
        let mut at = 0;
        for &(offset, op) in rows {
            self.advance(offset - at);
            at = offset;
            self.row(op);
        }
        self.pad(start);
    }

    /// Where the header and the function are, in the debugger's copy.
    ///
    /// Both are addresses rather than distances and both are left to the linker. The header is an
    /// offset into this section, which moves when a link puts another object's section in front
    /// of it, so it is a relocation against the section itself. The function is its address and
    /// its length, each the width of a pointer, with no augmentation after them because the
    /// header asked for none.
    fn debug_record(&mut self, func: &Extent) {
        let word = self.align;
        self.out.relocs.push(Reloc {
            at: self.out.bytes.len(),
            symbol: DEBUG_FRAME.to_owned(),
            kind: Reference::Address { bytes: 4 },
            addend: i64::try_from(self.cie).expect("an object this size"),
            after: 0,
        });
        self.out.bytes.extend_from_slice(&0u32.to_le_bytes());
        self.out.relocs.push(Reloc {
            at: self.out.bytes.len(),
            symbol: func.name.clone(),
            kind: Reference::Address { bytes: u8::try_from(word).expect("a pointer width") },
            addend: 0,
            after: 0,
        });
        self.out.bytes.resize(self.out.bytes.len() + word, 0);
        let len = u64::try_from(func.len).expect("a function this size").to_le_bytes();
        self.out.bytes.extend_from_slice(&len[..word]);
    }

    /// Where the header and the function are, in the unwind table.
    fn eh_record(&mut self, func: &Extent) {
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
        // promised a length would be there. A function with a landing pad has four bytes, the
        // distance to its call site table, since its header said `L`.
        match self.lsda.take() {
            None => uleb(&mut self.out.bytes, 0),
            Some(Lsda::At(lsda)) => {
                uleb(&mut self.out.bytes, 4);
                self.out.relocs.push(Reloc {
                    at: self.out.bytes.len(),
                    symbol: EXCEPT_TABLE.to_owned(),
                    kind: Reference::Data,
                    addend: i64::try_from(lsda).expect("a table this size"),
                    after: 0,
                });
                self.out.bytes.extend_from_slice(&0u32.to_le_bytes());
            }
            Some(Lsda::Named(encoding, name)) => {
                let word = u8::try_from(self.align).expect("a pointer width");
                let size =
                    pointer_size(encoding, word, false).expect("an encoding the reader checked");
                uleb(&mut self.out.bytes, u64::from(size));
                self.pointer(encoding, &name, size);
            }
        }
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
    /// data alignment is for and what makes most of these two bytes long. It comes out positive
    /// for a slot below the end of the frame, because the alignment is negative. A slot above it
    /// is the one form with a signed distance, as gas writes it. Only a signal frame has one, where
    /// the kernel's vdso says the registers are in the `sigcontext` the frame points into.
    fn saved(&mut self, reg: u16, offset: i32) {
        let factored = i64::from(offset) / self.slot;
        debug_assert_eq!(
            factored * self.slot,
            i64::from(offset),
            "a slot is a whole number of slots below the end of the frame"
        );
        let Ok(factored) = u64::try_from(factored) else {
            self.out.bytes.push(OFFSET_EXTENDED_SF);
            uleb(&mut self.out.bytes, u64::from(reg));
            sleb(&mut self.out.bytes, factored);
            return;
        };
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
const SET_FPREG: u8 = 3;
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
    /// The frame pointer was pointed at the stack pointer, which is the one address the slots are
    /// counted from. Which register it is is a field of the header rather than part of the code, so
    /// there is nothing to carry here.
    Frame,
    /// A register went into a slot, named as DWARF numbers it, that far below the end of the frame.
    Save { reg: u16, from: i64 },
}

/// One code of a Windows prologue, in the terms both the record and gas's `.seh_` directives use.
///
/// This is what a listing and an object have to agree on. The code generator's rows are turned
/// into these once, by [`prologue`], and then either written into a record or printed as the
/// directive that asks an assembler for the same code, and a file of assembly is read back into
/// these from those directives. So a function that goes through a listing and one that does not
/// are described by the same codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Seh {
    /// `.seh_pushreg`: a register went on the stack, by the machine's number for it.
    Push(u8),
    /// `.seh_stackalloc`: the frame was taken, that many bytes of it.
    Alloc(i64),
    /// `.seh_setframe`: the frame register was pointed that far above the stack pointer.
    Frame { reg: u8, offset: i64 },
    /// `.seh_savereg` and `.seh_savexmm`: a register went into a slot that far above where the
    /// stack pointer ends the prologue.
    Save { reg: u8, vector: bool, above: i64 },
}

impl std::fmt::Display for Seh {
    /// The directive gas reads this code from, in the spelling gcc writes it in.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Seh::Push(reg) => write!(f, "\t.seh_pushreg\t%{}", GPR[usize::from(reg & 15)]),
            Seh::Alloc(size) => write!(f, "\t.seh_stackalloc\t{size}"),
            Seh::Frame { reg, offset } => {
                write!(f, "\t.seh_setframe\t%{}, {offset}", GPR[usize::from(reg & 15)])
            }
            Seh::Save { reg, vector: false, above } => {
                write!(f, "\t.seh_savereg\t%{}, {above}", GPR[usize::from(reg & 15)])
            }
            Seh::Save { reg, vector: true, above } => {
                write!(f, "\t.seh_savexmm\t%xmm{reg}, {above}")
            }
        }
    }
}

/// The general purpose registers, by the number the machine and the codes give them.
pub(crate) const GPR: [&str; 16] = [
    "rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi", "r8", "r9", "r10", "r11", "r12", "r13",
    "r14", "r15",
];

/// One function's prologue as codes: where the prologue ends, and each code with where the
/// instruction that did it ended, both counted from the front of the function.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Prologue {
    pub(crate) end: usize,
    pub(crate) codes: Vec<(usize, Seh)>,
}

/// The table Windows wants: one row per function in one section, and the description each row
/// points at in another.
///
/// A description is reached by name rather than by distance, because the row and the description
/// are in two sections and there is no distance between two sections a compilation can work out.
/// The name is local, since what it points at is one function's prologue and no other object has
/// any use for it.
fn windows(funcs: &[Extent], rows: &[Rows], conv: &CallRegs) -> Result<Unwind, Error> {
    let prologues = funcs
        .iter()
        .zip(rows)
        .map(|(func, rows)| {
            let codes = prologue(&func.name, rows, conv)?;
            // Where the last instruction that touched the frame ended, rather than where the last
            // instruction of the prologue ended. The two differ by the pieces that describe
            // nothing, which is the canary and the call to a profiler's hook, and what the number
            // is for is telling an address inside the prologue from one after it. An address in
            // those trailing pieces is one where the frame is already whole, so it is the right
            // answer for both.
            let end = codes.last().map_or(0, |(at, _)| *at);
            Ok(Prologue { end, codes })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    seh(funcs, &prologues)
}

/// The same table, from prologues already turned into codes, which is where a file of assembly
/// that said them with `.seh_` directives joins in.
pub(crate) fn seh(funcs: &[Extent], prologues: &[Prologue]) -> Result<Unwind, Error> {
    debug_assert_eq!(funcs.len(), prologues.len(), "a record per function");
    let mut out = Unwind::default();
    for (func, prologue) in funcs.iter().zip(prologues) {
        // The description first, because the row that points at it needs a name to point at and
        // the name is where the description landed.
        let name = format!("$unwind${}", func.name);
        let at = out.info.len();
        describe(&mut out.info, &func.name, prologue)?;
        out.labels.push(Marker { name: name.clone(), at });
        // The row: where the function starts, one past where it ends, and where its description is.
        // None of the three is a number this compilation knows, since all of them are placed by the
        // linker, so each is four zero bytes and a relocation saying how far the thing is from the
        // front of the image. The second is the first plus the length, which the relocation carries
        // as its addend rather than as a second symbol at the end of the function.
        let len = i64::try_from(func.len).expect("a function this size");
        for (symbol, addend) in [(func.name.clone(), 0), (func.name.clone(), len), (name, 0)] {
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
/// from, which is the frame pointer in a function that keeps one and nothing in a function that
/// does not.
fn describe(info: &mut Vec<u8>, func: &str, prologue: &Prologue) -> Result<(), Error> {
    let codes = prologue
        .codes
        .iter()
        .map(|&(at, code)| nodes(func, at, code))
        .collect::<Result<Vec<_>, _>>()?;
    let end = u8::try_from(prologue.end).map_err(|_| {
        frame(func, "a prologue longer than a record can count in a byte".to_owned())
    })?;
    let nodes = codes.iter().map(Vec::len).sum::<usize>() / 2;
    let count = u8::try_from(nodes).map_err(|_| {
        let why = format!("a prologue of {nodes} unwind slots, more than a record holds");
        frame(func, why)
    })?;
    // The machine's number for the frame register in the low four bits and how far above the end
    // of the prologue it was left, in sixteens, in the four above that. Zero in a function that
    // keeps no frame pointer, which is a register number no unwinder reads because a header that
    // names one says so in a code as well.
    let mut base = 0;
    for (_, code) in &prologue.codes {
        if let Seh::Frame { reg, offset } = *code {
            if !(0..=240).contains(&offset) || offset % 16 != 0 {
                let why = format!("a frame register {offset} bytes up, which a record cannot say");
                return Err(frame(func, why));
            }
            base = reg | u8::try_from(offset / 16).expect("checked just above") << 4;
        }
    }
    info.extend_from_slice(&[1, end, count, base]);
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
/// Every code carries where the instruction that did it was, which is whatever the rows say: a
/// byte offset for the object writer, where it is where the instruction ended, and an instruction's
/// place in the function for the listing, which prints the directive after that instruction and
/// leaves the counting to the assembler.
pub(crate) fn prologue(
    func: &str,
    rows: &[(usize, CfiOp)],
    conv: &CallRegs,
) -> Result<Vec<(usize, Seh)>, Error> {
    let end = rows.iter().position(|(_, op)| *op == CfiOp::RememberState).unwrap_or(rows.len());
    let rows = &rows[..end];
    let word = i64::from(conv.word);
    // What the rows call the frame pointer, or nothing on a target no register of has a number,
    // which is a target this format is not written for anyway.
    let pointer = conv.dwarf(conv.int_class, conv.frame_pointer);
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
        match group {
            // A push, which says two things about one instruction: the end of the frame is a word
            // further up, and the register went in the word it just moved past. One code says both.
            [(_, CfiOp::DefCfaOffset(moved)), (_, CfiOp::Offset { reg, offset })]
                if i64::from(*moved) - below == word
                    && i64::from(*offset) == -i64::from(*moved) =>
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
            // The frame pointer, pointed at the stack pointer once the frame is whole. The code says
            // which register it is and how far above the end of the prologue it was left, and the
            // runtime reads both the other way round: it takes the register the function is stopped
            // in, subtracts that distance, and has the one address every slot below is measured
            // from. Here the distance is nothing, since the prologue points it at the stack pointer
            // itself, and the row saying so is the whole frame rather than a change of register,
            // which is how this tells the shape it can describe from the one it cannot.
            [(_, CfiOp::DefCfa { reg, offset })]
                if Some(*reg) == pointer && i64::from(*offset) == below =>
            {
                steps.push((at, Step::Frame));
            }
            // The other order, which is the pointer established before the frame is taken. What
            // cannot be said is not the pointer itself but what this compiler does after
            // establishing it: the registers it saves next sit below the place the codes would count
            // from, and the frame it takes afterwards has no row at all, since from there on the
            // rules are counted from the pointer and the stack pointer moving no longer changes
            // them. A record without the frame in it is a record that unwinds to the wrong place, so
            // it is refused instead. Nothing this compiler writes for Windows arrives here any more,
            // since a realigned frame takes the late order too, so this is the backstop.
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
    steps
        .into_iter()
        .map(|(at, step)| {
            let code = match step {
                Step::Push(reg) => Seh::Push(machine(func, conv, reg)?),
                Step::Alloc(size) => Seh::Alloc(size),
                Step::Frame => {
                    let reg = pointer.expect("a row that named the frame pointer");
                    Seh::Frame { reg: machine(func, conv, reg)?, offset: 0 }
                }
                Step::Save { reg, from } => Seh::Save {
                    reg: machine(func, conv, reg)?,
                    vector: conv.machine(conv.int_class, reg).is_none(),
                    above: below + from,
                },
            };
            Ok((at, code))
        })
        .collect()
}

/// How big a slot is on the one machine this format is written for, which is what the two codes
/// that count in slots divide by.
const WORD: i64 = 8;

/// One code, as the nodes that say it.
fn nodes(func: &str, at: usize, code: Seh) -> Result<Vec<u8>, Error> {
    let at = u8::try_from(at).map_err(|_| {
        frame(func, "a prologue longer than a record can count in a byte".to_owned())
    })?;
    match code {
        Seh::Push(reg) => Ok(vec![at, PUSH_NONVOL | reg << 4]),
        Seh::Alloc(size) if (WORD..=SMALL_FRAME).contains(&size) && size % WORD == 0 => {
            let steps = u8::try_from(size / WORD - 1).expect("a frame this small");
            Ok(vec![at, ALLOC_SMALL | steps << 4])
        }
        Seh::Alloc(size) => large(func, at, size),
        // The four bits the other codes put an operand in are reserved here, so they are nothing.
        Seh::Frame { .. } => Ok(vec![at, SET_FPREG]),
        Seh::Save { reg, vector, above } => slot(func, at, reg, vector, above),
    }
}

/// A frame too big for the code that holds one in four bits, in the two forms that hold a larger
/// one.
///
/// The first counts in slots and fits a frame of half a megabyte in one extra node. The second
/// counts in bytes and takes two, which is every frame a program on this machine can have, since a
/// thread's stack is not four gigabytes.
fn large(func: &str, at: u8, size: i64) -> Result<Vec<u8>, Error> {
    if size <= 0 || size % WORD != 0 {
        let why = format!("a frame of {size} bytes, not a whole number of slots");
        return Err(frame(func, why));
    }
    let mut out = vec![at, ALLOC_LARGE];
    if let Ok(slots) = u16::try_from(size / WORD) {
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
fn slot(func: &str, at: u8, reg: u8, vector: bool, above: i64) -> Result<Vec<u8>, Error> {
    if above < 0 {
        let why = format!("a register saved {} bytes below its own frame", -above);
        return Err(frame(func, why));
    }
    let bytes = u32::try_from(above).map_err(|_| {
        frame(func, format!("a register saved {above} bytes up, further than a record reaches"))
    })?;
    let (near, far, step) = if vector {
        (SAVE_XMM128, SAVE_XMM128_FAR, 16)
    } else {
        (SAVE_NONVOL, SAVE_NONVOL_FAR, WORD as u32)
    };
    let scaled = (above % i64::from(step) == 0).then(|| u16::try_from(bytes / step).ok()).flatten();
    let mut out = vec![at, if scaled.is_some() { near } else { far } | reg << 4];
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
fn machine(func: &str, conv: &CallRegs, reg: u16) -> Result<u8, Error> {
    let found = conv
        .machine(conv.int_class, reg)
        .or_else(|| conv.machine(conv.sse_class, reg))
        .map(|reg| reg.number())
        .filter(|number| *number < 16);
    found.ok_or_else(|| {
        frame(
            func,
            format!("a register saved under DWARF number {reg}, which this machine has none of"),
        )
    })
}

/// A prologue this cannot describe, named by the function it is the prologue of.
fn frame(func: &str, why: String) -> Error {
    Error::Frame { func: func.to_owned(), why }
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
pub(crate) fn uleb(bytes: &mut Vec<u8>, mut value: u64) {
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
pub(crate) fn sleb(bytes: &mut Vec<u8>, mut value: i64) {
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
    use rucc_target::x86_64::{SYSV, WIN64};

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
            landings: Vec::new(),
        }
    }

    /// The description one function's rows come out as, with the header and the padding.
    fn info(rows: Rows) -> Vec<u8> {
        let out = table(&[func("f", 64)], &[rows], &WIN64, ObjectFormat::Coff, &[], &[])
            .expect("a prologue this can describe");
        out.info
    }

    /// Why a prologue was refused, for a prologue that was.
    fn refused(rows: Rows) -> String {
        let out = table(&[func("f", 64)], &[rows], &WIN64, ObjectFormat::Coff, &[], &[])
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
        let want = [
            // Version one and no flags, a prologue of eight bytes, three nodes, and no register
            // the frame is counted from.
            vec![1, 8, 3, 0],
            // Thirty two bytes of frame, which is four slots and is written as one less.
            vec![8, ALLOC_SMALL | (3 << 4)],
            // rbx, which is three to DWARF and three to the machine.
            vec![2, PUSH_NONVOL | (3 << 4)],
            // rbp, which is six to DWARF and five to the machine.
            vec![1, PUSH_NONVOL | (5 << 4)],
            // An odd number of nodes, padded so the next record starts on a word.
            vec![0, 0],
        ];
        assert_eq!(info(rows), want.concat());
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
        let want = [
            vec![1, 14, 4, 0],
            // xmm6 at sixteen bytes up, which the code counts in sixteens.
            vec![14, SAVE_XMM128 | (6 << 4), 0x01, 0x00],
            vec![8, ALLOC_SMALL | (4 << 4)],
            vec![1, PUSH_NONVOL | (5 << 4)],
        ];
        assert_eq!(info(rows), want.concat());
    }

    /// A general purpose register in a slot, in the same shape, counted in words instead.
    #[test]
    fn a_general_purpose_register_in_a_slot_counts_in_words() {
        let rows = vec![
            (8, CfiOp::DefCfaOffset(72)),
            (13, CfiOp::Offset { reg: R12, offset: -48 }),
            (13, CfiOp::RememberState),
        ];
        let want = [
            vec![1, 13, 3, 0],
            // r12, which is twelve to both, three words up.
            vec![13, SAVE_NONVOL | (12 << 4), 0x03, 0x00],
            vec![8, ALLOC_SMALL | (7 << 4)],
            vec![0, 0],
        ];
        assert_eq!(info(rows), want.concat());
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
        let out = table(&funcs, &[Vec::new(), Vec::new()], &WIN64, ObjectFormat::Coff, &[], &[])
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
        let labels: Vec<_> =
            out.labels.iter().map(|label| (label.name.as_str(), label.at)).collect();
        assert_eq!(labels, vec![("$unwind$one", 0), ("$unwind$two", 4)]);
    }

    /// The frame pointer form the prologue writes on this platform: the pushes, the frame, and only
    /// then the pointer, which is what lets the record name a register every slot is counted from.
    ///
    /// The register goes in the header rather than in a code, and the four bits above it are how far
    /// above the end of the prologue the pointer was left, which is nothing here because the
    /// prologue points it at the stack pointer itself.
    #[test]
    fn a_frame_pointer_pointed_at_the_finished_frame_is_a_register_the_header_names() {
        let rows = vec![
            (1, CfiOp::DefCfaOffset(16)),
            (1, CfiOp::Offset { reg: RBP, offset: -16 }),
            (2, CfiOp::DefCfaOffset(24)),
            (2, CfiOp::Offset { reg: RBX, offset: -24 }),
            (9, CfiOp::DefCfaOffset(88)),
            (12, CfiOp::DefCfa { reg: RBP, offset: 88 }),
            (18, CfiOp::Offset { reg: XMM6, offset: -40 }),
            (18, CfiOp::RememberState),
        ];
        let want = [
            // rbp, which is five to the machine, and nothing above it.
            vec![1, 18, 6, 5],
            // Forty eight bytes up, which the code counts in sixteens.
            vec![18, SAVE_XMM128 | (6 << 4), 0x03, 0x00],
            // The pointer, whose four bits of operand are reserved and so are nothing.
            vec![12, SET_FPREG],
            vec![9, ALLOC_SMALL | (7 << 4)],
            vec![2, PUSH_NONVOL | (3 << 4)],
            vec![1, PUSH_NONVOL | (5 << 4)],
        ];
        assert_eq!(info(rows), want.concat());
    }

    /// The other order, which is refused rather than described. What cannot be said is not the
    /// pointer but the frame taken after it, which has no row and would come out as a record that
    /// unwinds to the wrong place.
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

    /// WebAssembly gets nothing rather than a table under a name its linker has never heard of,
    /// which would be the same bytes and the same failure to unwind with the size spent on it.
    #[test]
    fn a_format_whose_table_is_not_written_yet_gets_no_section() {
        let rows = vec![vec![(1, CfiOp::DefCfaOffset(16))]];
        let wasm =
            table(&[func("f", 8)], &rows, &WIN64, ObjectFormat::Wasm, &[], &[]).expect("nothing");
        assert_eq!(wasm, Unwind::default());
    }

    /// Mach-O gets ELF's records, and a function with a landing pad gets one without the routine
    /// and the call site table, which are spelled in ELF's terms. See [`table`].
    #[test]
    fn a_mach_o_table_is_the_dwarf_one_without_a_personality() {
        let rows = vec![vec![(1, CfiOp::DefCfaOffset(16))]];
        let elf = table(&[func("f", 8)], &rows, &SYSV, ObjectFormat::Elf, &[], &[]).unwrap();
        let mach = table(&[func("f", 8)], &rows, &SYSV, ObjectFormat::MachO, &[], &[]).unwrap();
        assert_eq!(mach, elf);
        let mut pad = func("f", 8);
        pad.landings = vec![rucc_object::Site { start: 0, len: 4, pad: 6 }];
        let mach = table(&[pad], &rows, &SYSV, ObjectFormat::MachO, &[], &[]).unwrap();
        assert_eq!(mach, elf);
    }

    /// The debugger's copy of a function that pushes its frame pointer: a header marked with all
    /// ones and no augmentation, and a record whose header and function are both addresses the
    /// linker fills in, the function's the width of a pointer and followed by its length.
    ///
    /// Byte for byte, because the layout is the whole of the difference from the unwind table and a
    /// reader given the wrong one reads every field after the first one it disagrees on as garbage.
    #[test]
    fn the_debuggers_copy_spells_the_header_and_the_function_as_addresses() {
        let rows = vec![vec![(1, CfiOp::DefCfaOffset(16))]];
        let frames =
            debug_frame(&[func("f", 9)], &rows, &SYSV, ObjectFormat::Elf, &[]).expect("a table");
        assert_eq!(frames.name, ".debug_frame");
        #[rustfmt::skip]
        let want: &[u8] = &[
            // The header: length, all ones, version one, no augmentation, the two alignments and
            // the return address column, then the frame ending at rsp+8 and the return address
            // the word below it, and nops out to eight bytes.
            20, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, 1, 0, 1, 0x78, 16,
            DEF_CFA, 7, 8, OFFSET | 16, 1, NOP, NOP, NOP, NOP, NOP, NOP,
            // The record: length, where the header is, where the function is and how long it is,
            // then one byte in the frame is sixteen bytes long.
            28, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0,
            9, 0, 0, 0, 0, 0, 0, 0,
            ADVANCE_LOC | 1, DEF_CFA_OFFSET, 16, NOP, NOP, NOP, NOP, NOP,
        ];
        assert_eq!(frames.bytes, want);
        let relocs: Vec<_> =
            frames.relocs.iter().map(|r| (r.at, r.symbol.as_str(), r.kind)).collect();
        assert_eq!(
            relocs,
            [
                (28, ".debug_frame", Reference::Address { bytes: 4 }),
                (32, "f", Reference::Address { bytes: 8 }),
            ]
        );
    }

    /// Nothing on a format that has no `.debug_frame`, for the reason the unwind table is nothing
    /// there either.
    #[test]
    fn the_debuggers_copy_is_only_written_on_elf() {
        let rows = vec![Vec::new()];
        assert_eq!(debug_frame(&[func("f", 8)], &rows, &WIN64, ObjectFormat::Coff, &[]), None);
    }
}
