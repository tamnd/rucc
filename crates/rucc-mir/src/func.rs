//! The function: its blocks, its instructions, and the tables they live in.
//!
//! Design: `spec/10-backend.md` section 10.1.
//!
//! One [`Func`] owns everything in it, the same shape `rucc-ir` uses and for the same reasons:
//! nothing is boxed, a reference to an instruction is a four-byte index, and the whole function
//! is dropped in one go.
//!
//! The instructions in a block are a doubly linked list rather than a run, because the passes
//! that run over MIR are the ones that insert most: the allocator writes spills and reloads
//! between existing instructions, edge moves appear after it, and the peepholes of
//! `spec/10-backend.md` section 10.9 delete. A run would move everything after each edit and
//! invalidate every index a pass was holding.
//!
//! A block's parameters and its successors are `Vec`s rather than runs in a pool, because both
//! grow after the block exists. Splitting a critical edge adds a block whose successor list is
//! written after it is created, and the allocator's live-range splitting adds parameters.
//!
//! # What the builder keeps
//!
//! An instruction's operands are in one order and only one: the ones it writes, then the ones it
//! reads, then the registers its memory operand names. The printer writes that order and the
//! parser rebuilds it, so a function whose operands are in some other order prints as text that
//! reads back as a different function. [`InstBuilder`] is what makes the order an invariant
//! rather than a rule every caller has to remember, which is why it is the only way to make an
//! instruction that is in a block.

use std::ops::{Index, IndexMut};

use rucc_base::{Idx, IdxRange, Symbol};
use rucc_diag::Span;
use rucc_target::RegClass;

use crate::inst::{
    Amode, Block, BlockCall, BlockData, Imm, ImmRef, Inst, InstData, InstLayout, Mem, MemRef,
    Opcode, Operand, OperandList, Param, Reg,
};

/// One function, in machine instructions.
#[derive(Debug)]
pub struct Func {
    /// The name it is called by, which is the name of the IR function it was lowered from.
    pub name: Symbol,

    insts: Vec<InstData>,
    inst_layout: Vec<InstLayout>,
    inst_spans: Vec<Span>,
    blocks: Vec<BlockData>,

    operands: Vec<Operand>,
    imms: Vec<Imm>,
    amodes: Vec<Amode>,
    /// The class of each virtual register, which is what says how many there are and what the
    /// allocator may put each of them in.
    vregs: Vec<RegClass>,

    first_block: Option<Block>,
    last_block: Option<Block>,
}

impl Func {
    /// A function of that name with nothing in it.
    #[must_use]
    pub fn new(name: Symbol) -> Self {
        Self {
            name,
            insts: Vec::new(),
            inst_layout: Vec::new(),
            inst_spans: Vec::new(),
            blocks: Vec::new(),
            operands: Vec::new(),
            imms: Vec::new(),
            amodes: Vec::new(),
            vregs: Vec::new(),
            first_block: None,
            last_block: None,
        }
    }

    // Registers.

    /// A virtual register of that class, which nothing has defined yet.
    ///
    /// # Panics
    ///
    /// Panics if the function already has two billion of them, which no function does.
    pub fn new_vreg(&mut self, class: RegClass) -> Reg {
        let number = u32::try_from(self.vregs.len()).expect("too many virtual registers");
        self.vregs.push(class);
        Reg::virtual_reg(number)
    }

    /// How many virtual registers the function has, which is what the allocator sizes itself
    /// against.
    #[must_use]
    pub fn vregs(&self) -> usize {
        self.vregs.len()
    }

    /// The class of a virtual register, or `None` for a physical one or a number this function
    /// never handed out.
    #[must_use]
    pub fn class_of(&self, reg: Reg) -> Option<RegClass> {
        self.vregs.get(usize::try_from(reg.number()?).ok()?).copied()
    }

    // Blocks.

    /// Creates a block with no parameters and nothing in it, at the end of the layout.
    pub fn create_block(&mut self) -> Block {
        let block = Idx::from_usize(self.blocks.len());
        self.blocks.push(BlockData { prev: self.last_block, ..BlockData::default() });
        match self.last_block {
            Some(last) => self.blocks[last.index()].next = Some(block),
            None => self.first_block = Some(block),
        }
        self.last_block = Some(block);
        block
    }

    /// The entry block, which is the first in layout order, or `None` before there is one.
    #[must_use]
    pub fn entry(&self) -> Option<Block> {
        self.first_block
    }

    /// How many blocks the function has ever had, which is what a table indexed by block is
    /// sized against. A block taken out of the layout still counts, because it keeps its index.
    #[must_use]
    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    /// How many instructions the function has ever had, which is what a table indexed by
    /// instruction is sized against. One taken out of a block still counts, because it keeps its
    /// index.
    #[must_use]
    pub fn inst_count(&self) -> usize {
        self.insts.len()
    }

    /// Its blocks, in layout order, which is the order they are printed and emitted in.
    pub fn blocks(&self) -> impl Iterator<Item = Block> + use<'_> {
        std::iter::successors(self.first_block, |&block| self[block].next)
    }

    /// Adds a parameter of that class to a block, and gives back the virtual register it
    /// arrives as.
    ///
    /// Every predecessor's arm has to grow an argument to match, which is what
    /// [`Func::succs_mut`] is for.
    pub fn append_param(&mut self, block: Block, class: RegClass) -> Reg {
        let reg = self.new_vreg(class);
        self.blocks[block.index()].params.push(Param { reg, class });
        reg
    }

    /// Adds a parameter that is already a particular register, which is what allocation leaves
    /// behind.
    pub fn append_given_param(&mut self, block: Block, param: Param) {
        self.blocks[block.index()].params.push(param);
    }

    /// What arrives in a block, to be read or replaced.
    ///
    /// Allocation is what replaces it: once every parameter is a place and every argument is a
    /// place, an edge is a set of moves and the parameters are what those moves write, so the
    /// block stops asking for anything and the machine IR stops being in SSA form.
    pub fn params_mut(&mut self, block: Block) -> &mut Vec<Param> {
        &mut self.blocks[block.index()].params
    }

    /// Where a block goes, to be read or replaced.
    ///
    /// The arms are in the order the terminator's own arms run, so the first is the arm a
    /// conditional branch takes when its condition holds.
    pub fn succs_mut(&mut self, block: Block) -> &mut Vec<BlockCall> {
        &mut self.blocks[block.index()].succs
    }

    // Instructions.

    /// The instructions of a block, in order.
    pub fn insts(&self, block: Block) -> impl Iterator<Item = Inst> + use<'_> {
        std::iter::successors(self[block].first_inst, |&inst| self.inst_layout[inst.index()].next)
    }

    /// The last instruction of a block, which is its terminator once it has one.
    #[must_use]
    pub fn terminator(&self, block: Block) -> Option<Inst> {
        self[block].last_inst
    }

    /// Which block an instruction is in, or `None` for one that has been taken out of its
    /// block.
    #[must_use]
    pub fn block_of(&self, inst: Inst) -> Option<Block> {
        self.inst_layout[inst.index()].block
    }

    /// Where an instruction came from.
    #[must_use]
    pub fn span(&self, inst: Inst) -> Span {
        self.inst_spans[inst.index()]
    }

    /// Starts an instruction at the end of that block.
    ///
    /// Nothing is added to the function until [`InstBuilder::finish`], so a builder that is
    /// dropped leaves no trace.
    pub fn build(&mut self, block: Block, opcode: Opcode) -> InstBuilder<'_> {
        InstBuilder {
            func: self,
            block: Some(block),
            opcode,
            operands: Vec::new(),
            imm: None,
            mem: None,
            symbol: None,
            span: Span::DUMMY,
        }
    }

    /// Puts an instruction that is in no block at the end of one.
    ///
    /// # Panics
    ///
    /// Panics if the instruction is already in a block, because an instruction in two blocks is
    /// the kind of thing that is found much later and somewhere else.
    pub fn append_inst(&mut self, block: Block, inst: Inst) {
        assert!(self.inst_layout[inst.index()].block.is_none(), "the instruction is in a block");
        let last = self.blocks[block.index()].last_inst;
        self.inst_layout[inst.index()] = InstLayout { block: Some(block), prev: last, next: None };
        match last {
            Some(last) => self.inst_layout[last.index()].next = Some(inst),
            None => self.blocks[block.index()].first_inst = Some(inst),
        }
        self.blocks[block.index()].last_inst = Some(inst);
    }

    /// Puts an instruction that is in no block immediately after another one.
    ///
    /// # Panics
    ///
    /// Panics if the instruction is already in a block, or if the one it is to follow is in
    /// none.
    pub fn insert_after(&mut self, after: Inst, inst: Inst) {
        assert!(self.inst_layout[inst.index()].block.is_none(), "the instruction is in a block");
        let layout = self.inst_layout[after.index()];
        let block = layout.block.expect("the instruction to insert after is in no block");
        self.inst_layout[inst.index()] =
            InstLayout { block: Some(block), prev: Some(after), next: layout.next };
        self.inst_layout[after.index()].next = Some(inst);
        match layout.next {
            Some(next) => self.inst_layout[next.index()].prev = Some(inst),
            None => self.blocks[block.index()].last_inst = Some(inst),
        }
    }

    /// Starts an instruction that will be in no block until something puts it in one.
    ///
    /// This is what a pass that inserts rather than appends builds with, and it hands the
    /// instruction to [`Func::prepend_inst`], [`Func::insert_before`] or [`Func::insert_after`].
    /// Everything else about it is the same, which is the point: the operand order is the
    /// builder's invariant wherever the instruction ends up.
    pub fn build_loose(&mut self, opcode: Opcode) -> InstBuilder<'_> {
        InstBuilder {
            func: self,
            block: None,
            opcode,
            operands: Vec::new(),
            imm: None,
            mem: None,
            symbol: None,
            span: Span::DUMMY,
        }
    }

    /// Puts an instruction that is in no block at the start of one, in front of everything in it.
    ///
    /// # Panics
    ///
    /// Panics if the instruction is already in a block.
    pub fn prepend_inst(&mut self, block: Block, inst: Inst) {
        assert!(self.inst_layout[inst.index()].block.is_none(), "the instruction is in a block");
        let first = self.blocks[block.index()].first_inst;
        self.inst_layout[inst.index()] = InstLayout { block: Some(block), prev: None, next: first };
        match first {
            Some(first) => self.inst_layout[first.index()].prev = Some(inst),
            None => self.blocks[block.index()].last_inst = Some(inst),
        }
        self.blocks[block.index()].first_inst = Some(inst);
    }

    /// Puts an instruction that is in no block immediately before another one.
    ///
    /// This is what a reload is: the instruction that wants the value has to see it already
    /// read in, so the load goes in front of it rather than behind whatever came before, which
    /// is the same place only when something came before.
    ///
    /// # Panics
    ///
    /// Panics if the instruction is already in a block, or if the one it is to precede is in
    /// none.
    pub fn insert_before(&mut self, before: Inst, inst: Inst) {
        assert!(self.inst_layout[inst.index()].block.is_none(), "the instruction is in a block");
        let layout = self.inst_layout[before.index()];
        let block = layout.block.expect("the instruction to insert before is in no block");
        self.inst_layout[inst.index()] =
            InstLayout { block: Some(block), prev: layout.prev, next: Some(before) };
        self.inst_layout[before.index()].prev = Some(inst);
        match layout.prev {
            Some(prev) => self.inst_layout[prev.index()].next = Some(inst),
            None => self.blocks[block.index()].first_inst = Some(inst),
        }
    }

    /// Takes an instruction out of its block, leaving it in the function's tables.
    ///
    /// It keeps its index, the way a removed block keeps its number, because renumbering would
    /// invalidate every index anything else was holding.
    pub fn remove_inst(&mut self, inst: Inst) {
        let layout = self.inst_layout[inst.index()];
        let Some(block) = layout.block else { return };
        match layout.prev {
            Some(prev) => self.inst_layout[prev.index()].next = layout.next,
            None => self.blocks[block.index()].first_inst = layout.next,
        }
        match layout.next {
            Some(next) => self.inst_layout[next.index()].prev = layout.prev,
            None => self.blocks[block.index()].last_inst = layout.prev,
        }
        self.inst_layout[inst.index()] = InstLayout::default();
    }

    // The tables.

    /// Puts a run of operands in the operand table and gives back the run.
    pub fn push_operands(&mut self, operands: &[Operand]) -> OperandList {
        let start = Idx::from_usize(self.operands.len());
        self.operands.extend_from_slice(operands);
        IdxRange::new(start, Idx::from_usize(self.operands.len()))
    }

    /// Puts an immediate in the immediate table.
    pub fn add_imm(&mut self, value: i64) -> ImmRef {
        self.imms.push(Imm(value));
        Idx::from_usize(self.imms.len() - 1)
    }

    /// Puts an addressing mode in the table of them.
    pub fn add_amode(&mut self, amode: Amode) -> MemRef {
        self.amodes.push(amode);
        Idx::from_usize(self.amodes.len() - 1)
    }

    /// Creates an instruction that is in no block yet.
    pub fn create_inst(&mut self, data: InstData, span: Span) -> Inst {
        self.insts.push(data);
        self.inst_layout.push(InstLayout::default());
        self.inst_spans.push(span);
        Idx::from_usize(self.insts.len() - 1)
    }
}

impl Index<Inst> for Func {
    type Output = InstData;

    fn index(&self, inst: Inst) -> &InstData {
        &self.insts[inst.index()]
    }
}

impl IndexMut<Inst> for Func {
    fn index_mut(&mut self, inst: Inst) -> &mut InstData {
        &mut self.insts[inst.index()]
    }
}

impl Index<Block> for Func {
    type Output = BlockData;

    fn index(&self, block: Block) -> &BlockData {
        &self.blocks[block.index()]
    }
}

impl Index<OperandList> for Func {
    type Output = [Operand];

    fn index(&self, list: OperandList) -> &[Operand] {
        &self.operands[list.as_usize_range()]
    }
}

impl IndexMut<OperandList> for Func {
    fn index_mut(&mut self, list: OperandList) -> &mut [Operand] {
        &mut self.operands[list.as_usize_range()]
    }
}

impl Index<ImmRef> for Func {
    type Output = Imm;

    fn index(&self, at: ImmRef) -> &Imm {
        &self.imms[at.index()]
    }
}

impl Index<MemRef> for Func {
    type Output = Amode;

    fn index(&self, at: MemRef) -> &Amode {
        &self.amodes[at.index()]
    }
}

/// One instruction being built.
///
/// The order the operands are given in is the order they are stored in, and the builder is what
/// insists that order is the one the printer and the parser agree on.
#[derive(Debug)]
pub struct InstBuilder<'a> {
    func: &'a mut Func,
    block: Option<Block>,
    opcode: Opcode,
    operands: Vec<Operand>,
    imm: Option<i64>,
    mem: Option<Amode>,
    symbol: Option<Symbol>,
    span: Span,
}

impl InstBuilder<'_> {
    /// Adds an operand.
    ///
    /// # Panics
    ///
    /// Panics if an operand the instruction writes is given after one it reads, or if either is
    /// given after the memory operand, because both make the instruction print as text that
    /// reads back as a different one.
    #[must_use]
    pub fn operand(mut self, operand: Operand) -> Self {
        assert!(self.mem.is_none(), "the memory operand's registers come last");
        if operand.role.is_def() {
            let reads = self.operands.iter().any(|earlier| !earlier.role.is_def());
            assert!(!reads, "the operands an instruction writes come first");
        }
        self.operands.push(operand);
        self
    }

    /// Adds the operand the instruction writes, in the common case where nothing constrains it.
    #[must_use]
    pub fn def(self, reg: Reg, class: RegClass) -> Self {
        self.operand(Operand::write(reg, class))
    }

    /// Adds an operand the instruction reads, in the common case where nothing constrains it.
    #[must_use]
    pub fn uses(self, reg: Reg, class: RegClass) -> Self {
        self.operand(Operand::read(reg, class))
    }

    /// Gives the instruction a memory operand, whose registers become its last operands.
    ///
    /// # Panics
    ///
    /// Panics if it already has one.
    #[must_use]
    pub fn mem(mut self, mem: Mem) -> Self {
        assert!(self.mem.is_none(), "the instruction already has a memory operand");
        let mut amode = Amode {
            base: None,
            index: None,
            scale: mem.scale.max(1),
            disp: mem.disp,
            symbol: mem.symbol,
        };
        if let Some(base) = mem.base {
            amode.base = Some(self.next_operand());
            self.operands.push(base);
        }
        if let Some(index) = mem.index {
            amode.index = Some(self.next_operand());
            self.operands.push(index);
        }
        self.mem = Some(amode);
        self
    }

    /// Gives the instruction an immediate.
    #[must_use]
    pub fn imm(mut self, value: i64) -> Self {
        self.imm = Some(value);
        self
    }

    /// Gives the instruction the symbol it names.
    #[must_use]
    pub fn symbol(mut self, symbol: Symbol) -> Self {
        self.symbol = Some(symbol);
        self
    }

    /// Says where in the source the instruction came from.
    #[must_use]
    pub fn at(mut self, span: Span) -> Self {
        self.span = span;
        self
    }

    /// Puts the instruction at the end of the block it was started in, or in no block at all if
    /// it was started loose.
    pub fn finish(self) -> Inst {
        let InstBuilder { func, block, opcode, operands, imm, mem, symbol, span } = self;
        let data = InstData {
            opcode,
            operands: func.push_operands(&operands),
            imm: imm.map(|value| func.add_imm(value)),
            mem: mem.map(|amode| func.add_amode(amode)),
            symbol,
        };
        let inst = func.create_inst(data, span);
        if let Some(block) = block {
            func.append_inst(block, inst);
        }
        inst
    }

    /// The index the next operand will have, for an addressing mode to point at.
    ///
    /// # Panics
    ///
    /// Panics past 255 operands, which is far more than any instruction of any target we have
    /// and which the index in an addressing mode could not name anyway.
    fn next_operand(&self) -> u8 {
        u8::try_from(self.operands.len()).expect("too many operands on one instruction")
    }
}

/// Whether an operand is one the instruction writes, for a printer or a pass that splits the
/// operand vector at the point the writes stop.
#[must_use]
pub fn defs(operands: &[Operand]) -> usize {
    operands.iter().position(|operand| !operand.role.is_def()).unwrap_or(operands.len())
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;

    use super::*;

    fn class() -> RegClass {
        RegClass::new(0)
    }

    #[test]
    fn instructions_come_back_in_the_order_they_were_built() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        let opcode = Opcode::new(names.intern("x64.nop"));
        let first = func.build(block, opcode).finish();
        let second = func.build(block, opcode).finish();
        assert_eq!(func.insts(block).collect::<Vec<_>>(), vec![first, second]);
        assert_eq!(func.terminator(block), Some(second));
        assert_eq!(func.block_of(first), Some(block));
    }

    #[test]
    fn a_removed_instruction_is_in_no_block_and_the_rest_still_link_up() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        let opcode = Opcode::new(names.intern("x64.nop"));
        let first = func.build(block, opcode).finish();
        let second = func.build(block, opcode).finish();
        let third = func.build(block, opcode).finish();
        func.remove_inst(second);
        assert_eq!(func.insts(block).collect::<Vec<_>>(), vec![first, third]);
        assert_eq!(func.block_of(second), None);
    }

    #[test]
    fn an_instruction_can_be_put_back_between_two_others() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        let opcode = Opcode::new(names.intern("x64.nop"));
        let first = func.build(block, opcode).finish();
        let last = func.build(block, opcode).finish();
        let spill = func.create_inst(InstData::new(opcode), Span::DUMMY);
        func.insert_after(first, spill);
        assert_eq!(func.insts(block).collect::<Vec<_>>(), vec![first, spill, last]);
        assert_eq!(func.terminator(block), Some(last));
    }

    #[test]
    fn an_instruction_can_be_put_in_front_of_the_first_one_in_a_block() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        let opcode = Opcode::new(names.intern("x64.nop"));
        let first = func.build(block, opcode).finish();
        let last = func.build(block, opcode).finish();
        let reload = func.create_inst(InstData::new(opcode), Span::DUMMY);
        let prologue = func.create_inst(InstData::new(opcode), Span::DUMMY);
        func.insert_before(last, reload);
        func.prepend_inst(block, prologue);
        assert_eq!(func.insts(block).collect::<Vec<_>>(), vec![prologue, first, reload, last]);
        assert_eq!(func.terminator(block), Some(last));
        assert_eq!(func.block_of(prologue), Some(block));
    }

    #[test]
    fn the_first_instruction_in_an_empty_block_is_also_its_last() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        let opcode = Opcode::new(names.intern("x64.ret"));
        let only = func.create_inst(InstData::new(opcode), Span::DUMMY);
        func.prepend_inst(block, only);
        assert_eq!(func.insts(block).collect::<Vec<_>>(), vec![only]);
        assert_eq!(func.terminator(block), Some(only));
    }

    #[test]
    fn a_memory_operand_names_the_operands_holding_its_registers() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        let base = func.new_vreg(class());
        let index = func.new_vreg(class());
        let dest = func.new_vreg(class());
        let inst = func
            .build(block, Opcode::new(names.intern("x64.lea")))
            .def(dest, class())
            .mem(
                Mem::at(Operand::read(base, class()))
                    .indexed(Operand::read(index, class()), 4)
                    .plus(16),
            )
            .finish();
        let data = func[inst];
        let amode = func[data.mem.expect("it was given a memory operand")];
        assert_eq!(amode.base, Some(1));
        assert_eq!(amode.index, Some(2));
        assert_eq!(amode.scale, 4);
        assert_eq!(amode.disp, 16);
        assert_eq!(func[data.operands][1].reg, base);
        assert_eq!(defs(&func[data.operands]), 1);
    }

    #[test]
    #[should_panic(expected = "the operands an instruction writes come first")]
    fn a_def_after_a_use_is_refused() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        let reg = func.new_vreg(class());
        let _ = func
            .build(block, Opcode::new(names.intern("x64.add")))
            .uses(reg, class())
            .def(reg, class());
    }

    #[test]
    fn a_block_parameter_is_a_virtual_register_of_its_class() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        let param = func.append_param(block, class());
        assert_eq!(func[block].params, vec![Param { reg: param, class: class() }]);
        assert_eq!(func.class_of(param), Some(class()));
        assert_eq!(func.vregs(), 1);
        assert_eq!(func.entry(), Some(block));
    }
}
