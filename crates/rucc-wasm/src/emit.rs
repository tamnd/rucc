//! The bytes of a function body, written one instruction at a time.
//!
//! The opcodes are the numbers of the binary format of the core specification, and each method
//! here writes one instruction with its immediates. A field that the linker patches is written at
//! its full padded width, five bytes, with a [`Fixup`] that says where it is, which is the form
//! the object writer and `wasm-ld` both expect.

use rucc_ir::Value;
use rucc_object::wasm::{self, Fixup, RelocKind, ValType};

/// The block type of a block, a loop or an `if` that takes nothing and gives nothing.
pub(crate) const EMPTY: u8 = 0x40;

pub(crate) const UNREACHABLE: u8 = 0x00;
pub(crate) const BLOCK: u8 = 0x02;
pub(crate) const LOOP: u8 = 0x03;
pub(crate) const IF: u8 = 0x04;
pub(crate) const ELSE: u8 = 0x05;
pub(crate) const END: u8 = 0x0b;
pub(crate) const RETURN: u8 = 0x0f;
pub(crate) const TRY_TABLE: u8 = 0x1f;
pub(crate) const DROP: u8 = 0x1a;
pub(crate) const SELECT: u8 = 0x1b;

pub(crate) const I32_LOAD: u8 = 0x28;
pub(crate) const I64_LOAD: u8 = 0x29;
pub(crate) const F32_LOAD: u8 = 0x2a;
pub(crate) const F64_LOAD: u8 = 0x2b;
pub(crate) const I32_LOAD8_S: u8 = 0x2c;
pub(crate) const I32_LOAD8_U: u8 = 0x2d;
pub(crate) const I32_LOAD16_S: u8 = 0x2e;
pub(crate) const I32_LOAD16_U: u8 = 0x2f;
pub(crate) const I32_STORE: u8 = 0x36;
pub(crate) const I64_STORE: u8 = 0x37;
pub(crate) const F32_STORE: u8 = 0x38;
pub(crate) const F64_STORE: u8 = 0x39;
pub(crate) const I32_STORE8: u8 = 0x3a;
pub(crate) const I32_STORE16: u8 = 0x3b;

pub(crate) const I32_EQZ: u8 = 0x45;
pub(crate) const I32_EQ: u8 = 0x46;
pub(crate) const I32_NE: u8 = 0x47;
pub(crate) const I32_LT_S: u8 = 0x48;
pub(crate) const I32_LT_U: u8 = 0x49;
pub(crate) const I32_GT_U: u8 = 0x4b;
pub(crate) const I64_EQZ: u8 = 0x50;
pub(crate) const I64_EQ: u8 = 0x51;
pub(crate) const I64_NE: u8 = 0x52;
pub(crate) const I64_LT_S: u8 = 0x53;
pub(crate) const I64_LT_U: u8 = 0x54;
pub(crate) const I64_GT_U: u8 = 0x56;

pub(crate) const I32_CLZ: u8 = 0x67;
pub(crate) const I32_CTZ: u8 = 0x68;
pub(crate) const I32_POPCNT: u8 = 0x69;
pub(crate) const I32_ADD: u8 = 0x6a;
pub(crate) const I32_SUB: u8 = 0x6b;
pub(crate) const I32_MUL: u8 = 0x6c;
pub(crate) const I32_DIV_S: u8 = 0x6d;
pub(crate) const I32_DIV_U: u8 = 0x6e;
pub(crate) const I32_REM_S: u8 = 0x6f;
pub(crate) const I32_REM_U: u8 = 0x70;
pub(crate) const I32_AND: u8 = 0x71;
pub(crate) const I32_OR: u8 = 0x72;
pub(crate) const I32_XOR: u8 = 0x73;
pub(crate) const I32_SHL: u8 = 0x74;
pub(crate) const I32_SHR_S: u8 = 0x75;
pub(crate) const I32_SHR_U: u8 = 0x76;
pub(crate) const I32_ROTL: u8 = 0x77;
pub(crate) const I32_ROTR: u8 = 0x78;

/// What an `i32` arithmetic opcode is plus this is the `i64` one, from `clz` to `rotr`.
pub(crate) const I64_FROM_I32: u8 = 0x12;

pub(crate) const F32_ABS: u8 = 0x8b;
pub(crate) const F32_NEG: u8 = 0x8c;
pub(crate) const F32_ADD: u8 = 0x92;
pub(crate) const F32_COPYSIGN: u8 = 0x98;
pub(crate) const F64_ABS: u8 = 0x99;
pub(crate) const F64_NEG: u8 = 0x9a;
pub(crate) const F64_ADD: u8 = 0xa0;
pub(crate) const F64_COPYSIGN: u8 = 0xa6;

pub(crate) const I32_WRAP_I64: u8 = 0xa7;
pub(crate) const I64_EXTEND_I32_S: u8 = 0xac;
pub(crate) const I64_EXTEND_I32_U: u8 = 0xad;
pub(crate) const F32_DEMOTE_F64: u8 = 0xb6;
pub(crate) const F64_PROMOTE_F32: u8 = 0xbb;
pub(crate) const I32_REINTERPRET_F32: u8 = 0xbc;
pub(crate) const I64_REINTERPRET_F64: u8 = 0xbd;
pub(crate) const F32_REINTERPRET_I32: u8 = 0xbe;
pub(crate) const F64_REINTERPRET_I64: u8 = 0xbf;
pub(crate) const I32_EXTEND8_S: u8 = 0xc0;
pub(crate) const I32_EXTEND16_S: u8 = 0xc1;

/// The prefix of the saturating conversions and of the bulk memory instructions.
const PREFIX_FC: u8 = 0xfc;
pub(crate) const MEMORY_COPY: u32 = 10;
pub(crate) const MEMORY_FILL: u32 = 11;

/// One function body as it is written, and the fields in it that the linker patches.
#[derive(Default)]
pub(crate) struct Code {
    pub(crate) bytes: Vec<u8>,
    pub(crate) fixups: Vec<Fixup>,
    /// The last `br`, while the code after it is only the `end`s that [`Code::end`] wrote.
    jump: Option<Jump>,
    /// Where the code after the last `return` or `unreachable` that [`Code::stop`] wrote starts.
    stop: Option<usize>,
    /// Each `local.set` and `local.tee`, in the order of the code, when the places of the
    /// declarations are asked for above `-O0`. [`Code::end`] removes only a `br` that has nothing
    /// after it but `end`s, so it never moves a write.
    pub(crate) writes: Option<Vec<Write>>,
}

/// One `local.set` or `local.tee` in the code.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Write {
    /// Where the instruction starts.
    pub(crate) from: u32,
    /// Where the instruction ends, which is where the local holds what it wrote.
    pub(crate) to: u32,
    pub(crate) local: u32,
    /// The value that the local holds after it, when the selector knows it.
    pub(crate) value: Option<Value>,
}

/// A `br` that can go when the code falls through to the same place.
#[derive(Clone, Copy)]
struct Jump {
    /// Where the `br` starts.
    at: usize,
    /// Where the code after the `br` starts.
    after: usize,
    depth: u32,
    /// How many frames the `end`s after the `br` close.
    ends: u32,
}

impl Code {
    pub(crate) fn op(&mut self, op: u8) {
        self.bytes.push(op);
    }

    /// A `return` or an `unreachable`, after which no code is reached.
    pub(crate) fn stop(&mut self, op: u8) {
        self.bytes.push(op);
        self.stop = Some(self.bytes.len());
    }

    /// Whether the last instruction is a `return` or an `unreachable` that [`Code::stop`] wrote.
    /// The stack after it can be of any type, so the `end` of the function body needs no other
    /// `unreachable` before it.
    pub(crate) fn stopped(&self) -> bool {
        self.stop == Some(self.bytes.len())
    }

    pub(crate) fn uleb(&mut self, value: u64) {
        wasm::uleb(&mut self.bytes, value);
    }

    pub(crate) fn sleb(&mut self, value: i64) {
        wasm::sleb(&mut self.bytes, value);
    }

    /// A field that the linker fills in, at its padded width, and the fixup that says so.
    pub(crate) fn reloc(&mut self, kind: RelocKind, target: u32, addend: i32) {
        let at = u32::try_from(self.bytes.len()).expect("a function body under 4 GiB");
        self.fixups.push(Fixup { at, kind, target, addend });
        let field = if matches!(
            kind,
            RelocKind::TableIndexSleb | RelocKind::MemoryAddrSleb | RelocKind::MemoryAddrTlsSleb
        ) {
            wasm::sleb_padded(0)
        } else {
            wasm::uleb_padded(0)
        };
        self.bytes.extend_from_slice(&field);
    }

    /// A block, a loop or an `if` that takes nothing and gives nothing, or one with a result.
    pub(crate) fn open(&mut self, op: u8, result: Option<ValType>) {
        self.bytes.push(op);
        self.bytes.push(result.map_or(EMPTY, ValType::byte));
    }

    /// A `try_table` that takes nothing and gives nothing, with one `catch` clause that sends an
    /// exception with the tag `tag` to the label at `depth`, with the values of the tag. The depth
    /// counts from the frame around the `try_table` and not from the `try_table` itself.
    pub(crate) fn try_table(&mut self, tag: u32, depth: u32) {
        self.bytes.extend_from_slice(&[TRY_TABLE, EMPTY, 1, 0x00]);
        self.reloc(RelocKind::TagIndexLeb, tag, 0);
        self.uleb(u64::from(depth));
    }

    pub(crate) fn br(&mut self, depth: u32) {
        let at = self.bytes.len();
        self.bytes.push(0x0c);
        self.uleb(u64::from(depth));
        self.jump = Some(Jump { at, after: self.bytes.len(), depth, ends: 0 });
    }

    /// The `end` of a block, a loop or an `if` that takes nothing and gives nothing, where `to_end`
    /// says that a branch to the frame goes to its end, which is false for a loop. When the code
    /// since the last `br` is only the `end`s of the frames inside the one that the `br` goes to,
    /// and this `end` closes that frame, the code falls through to the place where the `br` goes,
    /// so the `br` goes. Its bytes are given back, so that the notes after it can move.
    pub(crate) fn end(&mut self, to_end: bool) -> Option<std::ops::Range<usize>> {
        self.stop = None;
        let jump = self.jump.take().filter(|jump| {
            self.bytes.len() == jump.after + jump.ends as usize && jump.ends <= jump.depth
        });
        match jump {
            Some(jump) if jump.ends == jump.depth && to_end => {
                self.bytes.drain(jump.at..jump.after);
                self.bytes.push(END);
                Some(jump.at..jump.after)
            }
            Some(jump) => {
                self.bytes.push(END);
                if jump.ends < jump.depth {
                    self.jump = Some(Jump { ends: jump.ends + 1, ..jump });
                }
                None
            }
            None => {
                self.bytes.push(END);
                None
            }
        }
    }

    pub(crate) fn br_if(&mut self, depth: u32) {
        self.bytes.push(0x0d);
        self.uleb(u64::from(depth));
    }

    pub(crate) fn br_table(&mut self, depths: &[u32], default: u32) {
        self.bytes.push(0x0e);
        self.uleb(depths.len() as u64);
        for &depth in depths {
            self.uleb(u64::from(depth));
        }
        self.uleb(u64::from(default));
    }

    /// `call`, or `return_call` when `tail` is set.
    pub(crate) fn call(&mut self, symbol: u32, tail: bool) {
        self.bytes.push(if tail { 0x12 } else { 0x10 });
        self.reloc(RelocKind::FunctionIndexLeb, symbol, 0);
    }

    /// `call_indirect` through the table, or `return_call_indirect` when `tail` is set. The table
    /// index is the long form with a relocation when the target allows it and the single zero
    /// byte of the first version when it does not.
    pub(crate) fn call_indirect(&mut self, ty: u32, table: Option<u32>, tail: bool) {
        self.bytes.push(if tail { 0x13 } else { 0x11 });
        self.reloc(RelocKind::TypeIndexLeb, ty, 0);
        match table {
            Some(table) => self.reloc(RelocKind::TableNumberLeb, table, 0),
            None => self.bytes.push(0),
        }
    }

    pub(crate) fn local_get(&mut self, local: u32) {
        self.bytes.push(0x20);
        self.uleb(u64::from(local));
    }

    pub(crate) fn local_set(&mut self, local: u32) {
        let from = self.bytes.len();
        self.bytes.push(0x21);
        self.uleb(u64::from(local));
        self.wrote(from, local);
    }

    pub(crate) fn local_tee(&mut self, local: u32) {
        let from = self.bytes.len();
        self.bytes.push(0x22);
        self.uleb(u64::from(local));
        self.wrote(from, local);
    }

    fn wrote(&mut self, from: usize, local: u32) {
        let to = self.bytes.len();
        if let Some(writes) = self.writes.as_mut() {
            let offset = |at: usize| u32::try_from(at).expect("a function body under 4 GiB");
            writes.push(Write { from: offset(from), to: offset(to), local, value: None });
        }
    }

    pub(crate) fn global_get(&mut self, symbol: u32) {
        self.bytes.push(0x23);
        self.reloc(RelocKind::GlobalIndexLeb, symbol, 0);
    }

    pub(crate) fn global_set(&mut self, symbol: u32) {
        self.bytes.push(0x24);
        self.reloc(RelocKind::GlobalIndexLeb, symbol, 0);
    }

    /// A load or a store, with the alignment as a power of two and the offset.
    pub(crate) fn mem(&mut self, op: u8, align: u32, offset: u32) {
        self.bytes.push(op);
        self.uleb(u64::from(align));
        self.uleb(u64::from(offset));
    }

    /// A load or a store whose offset field is the address of data, which the linker writes.
    pub(crate) fn mem_at(&mut self, op: u8, align: u32, symbol: u32, addend: i32) {
        self.bytes.push(op);
        self.uleb(u64::from(align));
        self.reloc(RelocKind::MemoryAddrLeb, symbol, addend);
    }

    pub(crate) fn i32_const(&mut self, value: i32) {
        self.bytes.push(0x41);
        self.sleb(i64::from(value));
    }

    pub(crate) fn i64_const(&mut self, value: i64) {
        self.bytes.push(0x42);
        self.sleb(value);
    }

    pub(crate) fn f32_const(&mut self, bits: u32) {
        self.bytes.push(0x43);
        self.bytes.extend_from_slice(&bits.to_le_bytes());
    }

    pub(crate) fn f64_const(&mut self, bits: u64) {
        self.bytes.push(0x44);
        self.bytes.extend_from_slice(&bits.to_le_bytes());
    }

    /// An address the linker gives, of data or of a slot in the function table.
    pub(crate) fn address(&mut self, kind: RelocKind, symbol: u32, addend: i32) {
        self.bytes.push(0x41);
        self.reloc(kind, symbol, addend);
    }

    /// One instruction behind the `0xfc` prefix.
    pub(crate) fn prefixed(&mut self, op: u32) {
        self.bytes.push(PREFIX_FC);
        self.uleb(u64::from(op));
    }

    /// `memory.copy` or `memory.fill` on memory zero.
    pub(crate) fn bulk(&mut self, op: u32) {
        self.prefixed(op);
        if op == MEMORY_COPY {
            self.bytes.push(0);
        }
        self.bytes.push(0);
    }
}
