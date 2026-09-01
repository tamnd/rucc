//! The function: its blocks, its instructions, its values, and the tables they live in.
//!
//! Design: `spec/08-ir.md` sections 8.1 and 8.6.
//!
//! One [`Func`] owns everything in it. Nothing is boxed and nothing is individually freed: the
//! instructions are a flat vector, a reference to one is a four-byte index, and the whole
//! function is dropped in one go. The same shape as the AST, for the same reasons.
//!
//! Two things are not flat, and both for the same reason, which is that SSA construction
//! finishes a loop header long after it has built the blocks inside the loop.
//!
//! The instructions in a block are a doubly linked list rather than a run, because the
//! optimizer inserts and removes instructions constantly and a run would move every
//! instruction after the edit, invalidating every [`Inst`] anybody was holding.
//!
//! A block's parameters are a `Vec` rather than a run in a pool, because a run in a pool
//! cannot grow once something else has been put after it, and adding a parameter to a loop
//! header is exactly the operation that has to grow one.
//!
//! # CFG invariants
//!
//! The entry block has no predecessors and its parameters are the function's arguments in
//! their C-level form, before the ABI has been applied. Every other block ends in exactly one
//! terminator and contains no terminator anywhere else. These are checked by the verifier
//! rather than by the builder, because a function under construction breaks all of them and
//! the useful question is whether it still does when the pass that was building it says it has
//! finished.

use std::ops::{Index, IndexMut};

use rucc_base::{Idx, Symbol};
use rucc_diag::Span;

use crate::inst::{
    AsmInfo, Block, BlockCall, BlockCallList, BlockData, CallInfo, Def, Extra, Imm, ImmList, Inst,
    InstData, InstLayout, MemInfo, Sig, Signature, SwitchInfo, Value, ValueData, ValueList,
};
use crate::module::{Linkage, Visibility};
use crate::{Attrs, Flags, FloatPred, IntPred, Opcode, Type};

/// One function.
#[derive(Debug)]
pub struct Func {
    /// The name it is called by, which is what a direct call to it names.
    pub name: Symbol,
    /// How the linker sees it. `Internal` for a `static` function.
    pub linkage: Linkage,
    /// How the dynamic linker sees it.
    pub visibility: Visibility,
    /// The section to put it in, from `__attribute__((section(...)))`, or `None` to let the
    /// object writer choose.
    pub section: Option<Symbol>,
    /// What is true of the whole function, which is what a caller reads when it wants to know
    /// what a call to it does without looking inside.
    pub attrs: Attrs,

    values: Vec<ValueData>,
    insts: Vec<InstData>,
    inst_layout: Vec<InstLayout>,
    inst_spans: Vec<Span>,
    blocks: Vec<BlockData>,

    value_pool: Vec<Value>,
    block_calls: Vec<BlockCall>,
    imms: Vec<Imm>,
    mem: Vec<MemInfo>,
    calls: Vec<CallInfo>,
    switches: Vec<SwitchInfo>,
    asms: Vec<AsmInfo>,
    signatures: Vec<Signature>,

    first_block: Option<Block>,
    last_block: Option<Block>,
}

impl Func {
    /// A function with that name and that signature, and nothing in it.
    ///
    /// The signature becomes signature zero, which is what [`Func::signature`] gives back. The
    /// entry block is not created here, because the caller is about to create it and give it
    /// the parameters, and a half-built entry block is worse than no entry block. So a
    /// function fresh from here is a declaration, and stops being one when it gets a block.
    #[must_use]
    pub fn new(name: Symbol, signature: Signature) -> Self {
        Self {
            name,
            linkage: Linkage::External,
            visibility: Visibility::Default,
            section: None,
            attrs: Attrs::NONE,
            values: Vec::new(),
            insts: Vec::new(),
            inst_layout: Vec::new(),
            inst_spans: Vec::new(),
            blocks: Vec::new(),
            value_pool: Vec::new(),
            block_calls: Vec::new(),
            imms: Vec::new(),
            mem: Vec::new(),
            calls: Vec::new(),
            switches: Vec::new(),
            asms: Vec::new(),
            signatures: vec![signature],
            first_block: None,
            last_block: None,
        }
    }

    /// Its own signature.
    #[must_use]
    pub fn signature(&self) -> &Signature {
        &self.signatures[0]
    }

    /// Records a signature a `call_indirect` is made with, and gives back its index.
    pub fn add_signature(&mut self, signature: Signature) -> Sig {
        self.signatures.push(signature);
        Idx::from_usize(self.signatures.len() - 1)
    }

    /// The entry block, which is the first one in layout order.
    ///
    /// `None` only before one has been created. The verifier is what insists a finished
    /// function has one.
    #[must_use]
    pub fn entry(&self) -> Option<Block> {
        self.first_block
    }

    /// Whether this only says the function exists somewhere, which is a function with no
    /// blocks in it.
    ///
    /// `extern int puts(const char *);` and every other declaration of something defined in
    /// another object is one of these, and it is here rather than left out of the module
    /// because a call needs its signature and its linkage.
    #[must_use]
    pub fn is_declaration(&self) -> bool {
        self.first_block.is_none()
    }

    // Blocks.

    /// Creates a block with no parameters and no instructions, at the end of the layout.
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

    /// Adds a parameter of that type to a block, and gives back the value it arrives as.
    ///
    /// Every predecessor's branch has to grow an argument to match, which is
    /// [`Func::append_arg`], and the verifier is what notices if one of them did not.
    ///
    /// # Panics
    ///
    /// Panics if the block already has four billion parameters, which no block does.
    pub fn append_param(&mut self, block: Block, ty: Type) -> Value {
        let index = u32::try_from(self.blocks[block.index()].params.len())
            .expect("a block with four billion parameters");
        let value = self.add_value(ValueData { ty, def: Def::Param { block, index } });
        self.blocks[block.index()].params.push(value);
        value
    }

    /// Drops the parameters of a block that a predicate turns down, and renumbers the rest.
    ///
    /// The predicate is asked about each parameter in the order the block takes them. A
    /// parameter that goes has to take the argument in the same position out of every branch
    /// to the block, which is the caller's work rather than this method's, because only the
    /// caller knows which branches there are. This is what removing a redundant block
    /// parameter is, and SSA construction is the thing that makes them.
    ///
    /// # Panics
    ///
    /// Panics if the block has four billion parameters, which no block does.
    pub fn retain_params(&mut self, block: Block, mut keep: impl FnMut(Value) -> bool) {
        let mut params = std::mem::take(&mut self.blocks[block.index()].params);
        params.retain(|&value| keep(value));
        for (index, &value) in params.iter().enumerate() {
            let index = u32::try_from(index).expect("a block with four billion parameters");
            self.values[value.index()].def = Def::Param { block, index };
        }
        self.blocks[block.index()].params = params;
    }

    /// Every block, in layout order.
    pub fn blocks(&self) -> impl Iterator<Item = Block> + use<'_> {
        std::iter::successors(self.first_block, move |&block| self.blocks[block.index()].next)
    }

    /// Every instruction in a block, in order.
    pub fn insts(&self, block: Block) -> impl Iterator<Item = Inst> + use<'_> {
        std::iter::successors(self.blocks[block.index()].first, move |&inst| {
            self.inst_layout[inst.index()].next
        })
    }

    /// The last instruction of a block, which is its terminator once it is finished.
    #[must_use]
    pub fn terminator(&self, block: Block) -> Option<Inst> {
        self.blocks[block.index()].last.filter(|&inst| self.is_terminator(inst))
    }

    /// Whether control leaves the block at this instruction.
    ///
    /// A question for the function rather than for the instruction, because inline assembly is
    /// the one case where the opcode is not enough: `asm goto` has labels and everything else
    /// does not, and the labels are in the function's table rather than on the instruction.
    #[must_use]
    pub fn is_terminator(&self, inst: Inst) -> bool {
        let data = &self[inst];
        match data.extra {
            Extra::Asm(info) => {
                data.opcode.is_terminator() || !self.asms[info.index()].targets.is_empty()
            }
            _ => data.opcode.is_terminator(),
        }
    }

    // Instructions.

    /// Creates an instruction and its result values, without putting it in a block.
    ///
    /// The results are allocated here and are contiguous, which is what lets an instruction
    /// hold the first of them and a count rather than a list.
    ///
    /// # Panics
    ///
    /// Panics if `results` has more than 255 types, which no instruction in the set does.
    pub fn create_inst(&mut self, mut data: InstData, results: &[Type], span: Span) -> Inst {
        let inst = Idx::from_usize(self.insts.len());
        data.results = u8::try_from(results.len()).expect("an instruction with too many results");
        data.first_result = results.first().map(|_| Idx::from_usize(self.values.len()));
        for (index, &ty) in results.iter().enumerate() {
            let index = u8::try_from(index).expect("checked just above");
            self.add_value(ValueData { ty, def: Def::Result { inst, index } });
        }
        self.insts.push(data);
        self.inst_layout.push(InstLayout::default());
        self.inst_spans.push(span);
        inst
    }

    /// Puts an instruction at the end of a block.
    ///
    /// # Panics
    ///
    /// Panics if the instruction is already in a block. Moving one is removing it and
    /// appending it, and doing it by accident is how a linked list ends up in two pieces.
    pub fn append_inst(&mut self, block: Block, inst: Inst) {
        assert!(self.inst_layout[inst.index()].block.is_none(), "the instruction is in a block");
        let last = self.blocks[block.index()].last;
        self.inst_layout[inst.index()] = InstLayout { block: Some(block), prev: last, next: None };
        match last {
            Some(last) => self.inst_layout[last.index()].next = Some(inst),
            None => self.blocks[block.index()].first = Some(inst),
        }
        self.blocks[block.index()].last = Some(inst);
    }

    /// Puts an instruction immediately before another one, in the block that one is in.
    ///
    /// # Panics
    ///
    /// Panics if `inst` is already in a block, or if `before` is not in one.
    pub fn insert_before(&mut self, inst: Inst, before: Inst) {
        assert!(self.inst_layout[inst.index()].block.is_none(), "the instruction is in a block");
        let at = self.inst_layout[before.index()];
        let block = at.block.expect("the instruction to insert before is not in a block");
        self.inst_layout[inst.index()] =
            InstLayout { block: Some(block), prev: at.prev, next: Some(before) };
        self.inst_layout[before.index()].prev = Some(inst);
        match at.prev {
            Some(prev) => self.inst_layout[prev.index()].next = Some(inst),
            None => self.blocks[block.index()].first = Some(inst),
        }
    }

    /// Takes an instruction out of its block, leaving it and its results in the tables.
    ///
    /// The instruction is not deleted, because deleting it would move every instruction after
    /// it. A removed instruction is unreachable from any block and is dropped when the whole
    /// function is.
    ///
    /// # Panics
    ///
    /// Panics if the instruction is not in a block.
    pub fn remove_inst(&mut self, inst: Inst) {
        let at = self.inst_layout[inst.index()];
        let block = at.block.expect("the instruction is not in a block");
        match at.prev {
            Some(prev) => self.inst_layout[prev.index()].next = at.next,
            None => self.blocks[block.index()].first = at.next,
        }
        match at.next {
            Some(next) => self.inst_layout[next.index()].prev = at.prev,
            None => self.blocks[block.index()].last = at.prev,
        }
        self.inst_layout[inst.index()] = InstLayout::default();
    }

    /// The block an instruction is in, or `None` if it has been removed from one.
    #[must_use]
    pub fn block_of(&self, inst: Inst) -> Option<Block> {
        self.inst_layout[inst.index()].block
    }

    /// Where an instruction came from in the source.
    #[must_use]
    pub fn span(&self, inst: Inst) -> Span {
        self.inst_spans[inst.index()]
    }

    /// Where an instruction branches to, which is empty when it does not branch.
    ///
    /// This is the one place that knows a `switch` keeps its targets in a side table and
    /// `asm goto` in another one, so nothing walking the CFG has to.
    pub fn successors(&self, inst: Inst) -> impl Iterator<Item = BlockCall> + use<'_> {
        self.block_calls[self.target_list(inst).as_usize_range()].iter().copied()
    }

    /// Where a terminator keeps its targets, for something that edits them rather than reads
    /// them.
    ///
    /// [`Func::successors`] is what walking the CFG wants. This is what recording an edge
    /// wants, because an edge that will grow an argument later has to be named by its place in
    /// the table rather than by the block it went to.
    #[must_use]
    pub fn target_list(&self, inst: Inst) -> BlockCallList {
        match self[inst].extra {
            Extra::Targets(targets) => targets,
            Extra::Switch(info) => self.switches[info.index()].targets,
            Extra::Asm(info) => self.asms[info.index()].targets,
            _ => BlockCallList::EMPTY,
        }
    }

    // The pools.

    /// Records a run of value operands.
    pub fn push_values(&mut self, values: &[Value]) -> ValueList {
        let start = Idx::from_usize(self.value_pool.len());
        self.value_pool.extend_from_slice(values);
        ValueList::new(start, Idx::from_usize(self.value_pool.len()))
    }

    /// Adds one value to the end of a run, giving back the run it became.
    ///
    /// The run grows in place when nothing has been put after it, which is the case while a
    /// list is being built. Otherwise it is copied to the end and the old space is left
    /// behind, which is what makes adding a parameter to a loop header possible at all. That
    /// happens once per value carried around a loop, so the copying is not what costs.
    pub fn append_arg(&mut self, list: ValueList, value: Value) -> ValueList {
        let range = list.as_usize_range();
        if range.end == self.value_pool.len() {
            self.value_pool.push(value);
            return ValueList::new(Idx::from_usize(range.start), Idx::from_usize(range.end + 1));
        }
        let start = self.value_pool.len();
        self.value_pool.extend_from_within(range);
        self.value_pool.push(value);
        ValueList::new(Idx::from_usize(start), Idx::from_usize(self.value_pool.len()))
    }

    /// Replaces the values in a run, which is what substituting one definition for another is.
    ///
    /// A run is a run whether it is an instruction's operands or a branch's arguments, so this
    /// is the whole of the rewriting a substitution has to do.
    pub fn rewrite(&mut self, list: ValueList, mut with: impl FnMut(Value) -> Value) {
        for value in &mut self.value_pool[list.as_usize_range()] {
            *value = with(*value);
        }
    }

    /// Records a run of branch targets.
    pub fn push_block_calls(&mut self, calls: &[BlockCall]) -> BlockCallList {
        let start = Idx::from_usize(self.block_calls.len());
        self.block_calls.extend_from_slice(calls);
        BlockCallList::new(start, Idx::from_usize(self.block_calls.len()))
    }

    /// Replaces one branch target, which is what redirecting an edge is.
    pub fn set_block_call(&mut self, at: Idx<BlockCall>, call: BlockCall) {
        self.block_calls[at.index()] = call;
    }

    /// Records a run of case values.
    pub fn push_imms(&mut self, imms: &[Imm]) -> ImmList {
        let start = Idx::from_usize(self.imms.len());
        self.imms.extend_from_slice(imms);
        ImmList::new(start, Idx::from_usize(self.imms.len()))
    }

    /// Records a constant.
    pub fn add_imm(&mut self, imm: Imm) -> Idx<Imm> {
        self.imms.push(imm);
        Idx::from_usize(self.imms.len() - 1)
    }

    /// Records what an access does.
    pub fn add_mem(&mut self, info: MemInfo) -> Idx<MemInfo> {
        self.mem.push(info);
        Idx::from_usize(self.mem.len() - 1)
    }

    /// Records a call's callee and signature.
    pub fn add_call(&mut self, info: CallInfo) -> Idx<CallInfo> {
        self.calls.push(info);
        Idx::from_usize(self.calls.len() - 1)
    }

    /// Records a `switch`'s targets and case values.
    pub fn add_switch(&mut self, info: SwitchInfo) -> Idx<SwitchInfo> {
        self.switches.push(info);
        Idx::from_usize(self.switches.len() - 1)
    }

    /// Records an inline assembly instruction's template and constraints.
    pub fn add_asm(&mut self, info: AsmInfo) -> Idx<AsmInfo> {
        self.asms.push(info);
        Idx::from_usize(self.asms.len() - 1)
    }

    /// How many values, instructions and blocks there are, for a reader that wants to size
    /// something by them.
    #[must_use]
    pub fn counts(&self) -> Counts {
        Counts { values: self.values.len(), insts: self.insts.len(), blocks: self.blocks.len() }
    }

    fn add_value(&mut self, data: ValueData) -> Value {
        self.values.push(data);
        Idx::from_usize(self.values.len() - 1)
    }
}

/// How many of each thing a function holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Counts {
    /// Values, including the ones whose defining instruction has been removed.
    pub values: usize,
    /// Instructions, including the ones that have been removed from their block.
    pub insts: usize,
    /// Blocks.
    pub blocks: usize,
}

// Reading is indexing. There is one of these for each handle, so `func[inst]` and `func[value]`
// and `&func[args]` all work and none of them needs a method whose name says which table.
impl Index<Value> for Func {
    type Output = ValueData;

    fn index(&self, value: Value) -> &ValueData {
        &self.values[value.index()]
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

impl Index<Sig> for Func {
    type Output = Signature;

    fn index(&self, sig: Sig) -> &Signature {
        &self.signatures[sig.index()]
    }
}

impl Index<ValueList> for Func {
    type Output = [Value];

    fn index(&self, list: ValueList) -> &[Value] {
        &self.value_pool[list.as_usize_range()]
    }
}

impl Index<BlockCallList> for Func {
    type Output = [BlockCall];

    fn index(&self, list: BlockCallList) -> &[BlockCall] {
        &self.block_calls[list.as_usize_range()]
    }
}

impl Index<Idx<BlockCall>> for Func {
    type Output = BlockCall;

    fn index(&self, at: Idx<BlockCall>) -> &BlockCall {
        &self.block_calls[at.index()]
    }
}

impl Index<ImmList> for Func {
    type Output = [Imm];

    fn index(&self, list: ImmList) -> &[Imm] {
        &self.imms[list.as_usize_range()]
    }
}

impl Index<Idx<Imm>> for Func {
    type Output = Imm;

    fn index(&self, at: Idx<Imm>) -> &Imm {
        &self.imms[at.index()]
    }
}

impl Index<Idx<MemInfo>> for Func {
    type Output = MemInfo;

    fn index(&self, at: Idx<MemInfo>) -> &MemInfo {
        &self.mem[at.index()]
    }
}

impl Index<Idx<CallInfo>> for Func {
    type Output = CallInfo;

    fn index(&self, at: Idx<CallInfo>) -> &CallInfo {
        &self.calls[at.index()]
    }
}

impl Index<Idx<SwitchInfo>> for Func {
    type Output = SwitchInfo;

    fn index(&self, at: Idx<SwitchInfo>) -> &SwitchInfo {
        &self.switches[at.index()]
    }
}

impl Index<Idx<AsmInfo>> for Func {
    type Output = AsmInfo;

    fn index(&self, at: Idx<AsmInfo>) -> &AsmInfo {
        &self.asms[at.index()]
    }
}

/// A cursor that appends to the end of one block.
///
/// This is the shape lowering wants: it works on one block at a time, it appends, and it wants
/// the value back so it can use it in the next instruction. Everything here is a thin wrapper
/// over [`Func::create_inst`] and [`Func::append_inst`], and anything the wrappers do not
/// cover is done with those two directly.
#[derive(Debug)]
pub struct Builder<'a> {
    func: &'a mut Func,
    block: Block,
    span: Span,
}

impl<'a> Builder<'a> {
    /// A cursor appending to that block, with every instruction taking that source location.
    pub fn new(func: &'a mut Func, block: Block) -> Self {
        Self { func, block, span: Span::DUMMY }
    }

    /// The same cursor, with a source location for the instructions after this.
    #[must_use]
    pub fn at(mut self, span: Span) -> Self {
        self.span = span;
        self
    }

    /// Sets the source location for the instructions after this.
    pub fn set_span(&mut self, span: Span) {
        self.span = span;
    }

    /// The function being built.
    pub fn func(&mut self) -> &mut Func {
        self.func
    }

    /// The block being appended to.
    #[must_use]
    pub fn block(&self) -> Block {
        self.block
    }

    /// Appends an instruction as it is, and gives back its results.
    pub fn inst(&mut self, data: InstData, results: &[Type]) -> Inst {
        let inst = self.func.create_inst(data, results, self.span);
        self.func.append_inst(self.block, inst);
        inst
    }

    /// The one value an instruction produces.
    ///
    /// # Panics
    ///
    /// Panics if it did not produce exactly one.
    pub fn value(&mut self, data: InstData, ty: Type) -> Value {
        let inst = self.inst(data, &[ty]);
        self.func[inst].first_result.expect("one result was asked for")
    }

    /// An integer constant.
    ///
    /// # Panics
    ///
    /// Panics if `ty` is not an integer type.
    pub fn iconst(&mut self, ty: Type, value: i128) -> Value {
        let imm = self.func.add_imm(Imm::int(value, ty.lane()));
        self.value(InstData { extra: Extra::Imm(imm), ..InstData::new(Opcode::IConst) }, ty)
    }

    /// A floating point constant, given as the bits of its format.
    pub fn fconst(&mut self, ty: Type, bits: u128) -> Value {
        let imm = self.func.add_imm(Imm::from_bits(bits));
        self.value(InstData { extra: Extra::Imm(imm), ..InstData::new(Opcode::FConst) }, ty)
    }

    /// A two-operand instruction whose result has the type of its operands.
    pub fn binary(&mut self, opcode: Opcode, lhs: Value, rhs: Value, flags: Flags) -> Value {
        let ty = self.func[lhs].ty;
        let args = self.func.push_values(&[lhs, rhs]);
        self.value(InstData { args, flags, ..InstData::new(opcode) }, ty)
    }

    /// A one-operand instruction whose result has the type given.
    pub fn unary(&mut self, opcode: Opcode, arg: Value, ty: Type) -> Value {
        let args = self.func.push_values(&[arg]);
        self.value(InstData { args, ..InstData::new(opcode) }, ty)
    }

    /// An integer comparison, which produces one `i1` per lane.
    pub fn icmp(&mut self, pred: IntPred, lhs: Value, rhs: Value) -> Value {
        let ty = self.func[lhs].ty.with_lane(Type::I1);
        let args = self.func.push_values(&[lhs, rhs]);
        self.value(
            InstData { args, extra: Extra::IntPred(pred), ..InstData::new(Opcode::ICmp) },
            ty,
        )
    }

    /// A floating point comparison, which produces one `i1` per lane.
    pub fn fcmp(&mut self, pred: FloatPred, lhs: Value, rhs: Value, flags: Flags) -> Value {
        let ty = self.func[lhs].ty.with_lane(Type::I1);
        let args = self.func.push_values(&[lhs, rhs]);
        self.value(
            InstData { args, flags, extra: Extra::FloatPred(pred), ..InstData::new(Opcode::FCmp) },
            ty,
        )
    }

    /// A read of that type from that address.
    pub fn load(&mut self, ty: Type, addr: Value, info: MemInfo, flags: Flags) -> Value {
        let mem = self.func.add_mem(info);
        let args = self.func.push_values(&[addr]);
        self.value(
            InstData { args, flags, extra: Extra::Mem(mem), ..InstData::new(Opcode::Load) },
            ty,
        )
    }

    /// A write of a value to an address.
    pub fn store(&mut self, value: Value, addr: Value, info: MemInfo, flags: Flags) -> Inst {
        let mem = self.func.add_mem(info);
        let args = self.func.push_values(&[value, addr]);
        self.inst(
            InstData { args, flags, extra: Extra::Mem(mem), ..InstData::new(Opcode::Store) },
            &[],
        )
    }

    /// An unconditional branch.
    pub fn jump(&mut self, target: Block, args: &[Value]) -> Inst {
        let call = self.block_call(target, args);
        let targets = self.func.push_block_calls(&[call]);
        self.inst(InstData { extra: Extra::Targets(targets), ..InstData::new(Opcode::Jump) }, &[])
    }

    /// A two-way branch, taking the first target when the condition is one.
    pub fn br_if(
        &mut self,
        cond: Value,
        then_block: Block,
        then_args: &[Value],
        else_block: Block,
        else_args: &[Value],
    ) -> Inst {
        let then_call = self.block_call(then_block, then_args);
        let else_call = self.block_call(else_block, else_args);
        let targets = self.func.push_block_calls(&[then_call, else_call]);
        let args = self.func.push_values(&[cond]);
        self.inst(
            InstData { args, extra: Extra::Targets(targets), ..InstData::new(Opcode::BrIf) },
            &[],
        )
    }

    /// A return of the values the signature says.
    pub fn ret(&mut self, values: &[Value]) -> Inst {
        let args = self.func.push_values(values);
        self.inst(InstData { args, ..InstData::new(Opcode::Return) }, &[])
    }

    /// A place control does not reach.
    pub fn unreachable(&mut self) -> Inst {
        self.inst(InstData::new(Opcode::Unreachable), &[])
    }

    /// A direct call, with the results its signature says it produces.
    pub fn call(&mut self, callee: Symbol, signature: Sig, args: &[Value]) -> Inst {
        let info = self.func.add_call(CallInfo { callee: Some(callee), signature });
        let returns = self.func[signature].returns.clone();
        let args = self.func.push_values(args);
        self.inst(
            InstData { args, extra: Extra::Call(info), ..InstData::new(Opcode::Call) },
            &returns,
        )
    }

    fn block_call(&mut self, block: Block, args: &[Value]) -> BlockCall {
        BlockCall { block, args: self.func.push_values(args) }
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;

    use super::*;
    use crate::MemOrder;
    use crate::inst::BlockCallList;

    /// The example from the spec, near enough: a loop that sums one to n and stores it.
    fn sum() -> (Func, Block, Block, Block) {
        let mut names = Interner::new();
        let i32_ = Type::int(32);
        let mut func = Func::new(
            names.intern("sum"),
            Signature::new().with_params(&[i32_]).with_returns(&[i32_]),
        );

        let entry = func.create_block();
        let n = func.append_param(entry, i32_);
        let header = func.create_block();
        let acc = func.append_param(header, i32_);
        let i = func.append_param(header, i32_);
        let exit = func.create_block();
        let result = func.append_param(exit, i32_);

        let mut b = Builder::new(&mut func, entry);
        let zero = b.iconst(i32_, 0);
        let cmp = b.icmp(IntPred::Sle, n, zero);
        b.br_if(cmp, exit, &[zero], header, &[zero, zero]);

        let mut b = Builder::new(&mut func, header);
        let one = b.iconst(i32_, 1);
        let next = b.binary(Opcode::Add, i, one, Flags::NSW);
        let total = b.binary(Opcode::Add, acc, next, Flags::NSW);
        let done = b.icmp(IntPred::Sge, next, n);
        b.br_if(done, exit, &[total], header, &[total, next]);

        let mut b = Builder::new(&mut func, exit);
        b.ret(&[result]);

        (func, entry, header, exit)
    }

    #[test]
    fn the_blocks_come_back_in_the_order_they_were_made() {
        let (func, entry, header, exit) = sum();
        assert_eq!(func.blocks().collect::<Vec<_>>(), [entry, header, exit]);
        assert_eq!(func.entry(), Some(entry));
    }

    #[test]
    fn each_block_holds_what_was_appended_to_it() {
        let (func, entry, header, exit) = sum();
        let opcodes =
            |block| func.insts(block).map(|inst| func[inst].opcode.name()).collect::<Vec<_>>();
        assert_eq!(opcodes(entry), ["iconst", "icmp", "br_if"]);
        assert_eq!(opcodes(header), ["iconst", "add", "add", "icmp", "br_if"]);
        assert_eq!(opcodes(exit), ["return"]);
    }

    #[test]
    fn asm_ends_a_block_when_it_has_labels_and_not_otherwise() {
        // The labels are in the function's table, so the instruction on its own cannot answer
        // and anything asking it rather than the function would walk off the end of the block.
        let mut func = Func::new(Symbol::from_raw(0), Signature::new());
        let block = func.create_block();
        let plain = func.add_asm(AsmInfo {
            template: Symbol::from_raw(0),
            constraints: Symbol::from_raw(0),
            clobbers: Symbol::from_raw(0),
            targets: BlockCallList::EMPTY,
        });
        let call = BlockCall { block, args: ValueList::EMPTY };
        let targets = func.push_block_calls(&[call]);
        let labelled = func.add_asm(AsmInfo {
            template: Symbol::from_raw(0),
            constraints: Symbol::from_raw(0),
            clobbers: Symbol::from_raw(0),
            targets,
        });

        let mut make = |extra| {
            let data = InstData { extra, ..InstData::new(Opcode::InlineAsm) };
            func.create_inst(data, &[], Span::DUMMY)
        };
        let plain = make(Extra::Asm(plain));
        let labelled = make(Extra::Asm(labelled));
        assert!(!func.is_terminator(plain));
        assert!(func.is_terminator(labelled));
    }

    #[test]
    fn every_block_ends_in_its_terminator() {
        let (func, entry, header, exit) = sum();
        for block in [entry, header, exit] {
            let last = func.terminator(block).expect("a terminator");
            assert_eq!(Some(last), func.insts(block).last());
        }
    }

    #[test]
    fn a_branch_carries_the_arguments_the_block_takes() {
        let (func, entry, header, _) = sum();
        let br = func.terminator(entry).expect("a terminator");
        let calls: Vec<BlockCall> = func.successors(br).collect();
        assert_eq!(calls.len(), 2);
        // The loop header takes two parameters, so the branch to it passes two.
        assert_eq!(calls[1].block, header);
        assert_eq!(func[calls[1].args].len(), 2);
        assert_eq!(func[header].params.len(), 2);
        assert_eq!(func[calls[0].args].len(), 1);
    }

    #[test]
    fn a_value_knows_what_defined_it() {
        let (func, entry, _, _) = sum();
        let first = func.insts(entry).next().expect("an instruction");
        let value = func[first].first_result.expect("a result");
        assert_eq!(func[value].def, Def::Result { inst: first, index: 0 });
        assert_eq!(func[value].ty, Type::int(32));

        let param = func[entry].params[0];
        assert_eq!(func[param].def, Def::Param { block: entry, index: 0 });
    }

    #[test]
    fn a_comparison_produces_one_bit() {
        let (func, entry, _, _) = sum();
        let cmp = func.insts(entry).nth(1).expect("the comparison");
        let value = func[cmp].first_result.expect("a result");
        assert_eq!(func[value].ty, Type::I1);
        assert_eq!(func[cmp].extra, Extra::IntPred(IntPred::Sle));
    }

    #[test]
    fn flags_ride_along_on_the_instruction_that_was_given_them() {
        let (func, _, header, _) = sum();
        let add = func.insts(header).nth(1).expect("the addition");
        assert_eq!(func[add].flags, Flags::NSW);
        let cmp = func.insts(header).nth(3).expect("the comparison");
        assert_eq!(func[cmp].flags, Flags::NONE);
    }

    #[test]
    fn removing_an_instruction_takes_it_out_of_the_middle() {
        let (mut func, _, header, _) = sum();
        let add = func.insts(header).nth(1).expect("the addition");
        func.remove_inst(add);
        let opcodes: Vec<&str> = func.insts(header).map(|inst| func[inst].opcode.name()).collect();
        assert_eq!(opcodes, ["iconst", "add", "icmp", "br_if"]);
        assert_eq!(func.block_of(add), None);
    }

    #[test]
    fn removing_the_first_and_the_last_keeps_the_ends_right() {
        let (mut func, entry, _, _) = sum();
        let first = func.insts(entry).next().expect("an instruction");
        let last = func.terminator(entry).expect("a terminator");
        func.remove_inst(first);
        func.remove_inst(last);
        let opcodes: Vec<&str> = func.insts(entry).map(|inst| func[inst].opcode.name()).collect();
        assert_eq!(opcodes, ["icmp"]);
        assert_eq!(func[entry].first, func[entry].last);
    }

    #[test]
    fn removing_the_only_instruction_empties_the_block() {
        let (mut func, _, _, exit) = sum();
        let only = func.insts(exit).next().expect("an instruction");
        func.remove_inst(only);
        assert_eq!(func.insts(exit).count(), 0);
        assert_eq!(func[exit].first, None);
        assert_eq!(func[exit].last, None);
    }

    #[test]
    fn inserting_before_puts_it_in_the_right_place() {
        let (mut func, entry, _, _) = sum();
        let cmp = func.insts(entry).nth(1).expect("the comparison");
        let made = func.create_inst(InstData::new(Opcode::Unreachable), &[], Span::DUMMY);
        func.insert_before(made, cmp);
        let opcodes: Vec<&str> = func.insts(entry).map(|inst| func[inst].opcode.name()).collect();
        assert_eq!(opcodes, ["iconst", "unreachable", "icmp", "br_if"]);
    }

    #[test]
    fn inserting_before_the_first_makes_it_the_first() {
        let (mut func, entry, _, _) = sum();
        let first = func.insts(entry).next().expect("an instruction");
        let made = func.create_inst(InstData::new(Opcode::Unreachable), &[], Span::DUMMY);
        func.insert_before(made, first);
        assert_eq!(func.insts(entry).next(), Some(made));
        assert_eq!(func[entry].first, Some(made));
    }

    #[test]
    fn a_list_grows_in_place_while_it_is_the_last_thing_in_the_pool() {
        let mut func = Func::new(Symbol::from_raw(0), Signature::new());
        let block = func.create_block();
        let a = func.append_param(block, Type::int(32));
        let b = func.append_param(block, Type::int(32));
        let list = func.push_values(&[a]);
        let grown = func.append_arg(list, b);
        assert_eq!(func[grown], [a, b]);
        assert_eq!(grown.as_usize_range().start, list.as_usize_range().start);
    }

    #[test]
    fn a_list_is_copied_when_something_is_behind_it() {
        let mut func = Func::new(Symbol::from_raw(0), Signature::new());
        let block = func.create_block();
        let a = func.append_param(block, Type::int(32));
        let b = func.append_param(block, Type::int(32));
        let list = func.push_values(&[a, a]);
        let behind = func.push_values(&[b]);
        let grown = func.append_arg(list, b);
        assert_eq!(func[grown], [a, a, b]);
        assert_eq!(func[list], [a, a], "the old run is still readable");
        assert_eq!(func[behind], [b], "and so is what was behind it");
        assert_ne!(grown.as_usize_range().start, list.as_usize_range().start);
    }

    #[test]
    fn a_parameter_added_late_is_the_next_one_along() {
        // This is the shape SSA construction leaves: the loop header gains a parameter after
        // the blocks that branch to it already exist, and each of their branches grows an
        // argument to match.
        let (mut func, entry, header, _) = sum();
        let extra = func.append_param(header, Type::int(32));
        assert_eq!(func[header].params.len(), 3);
        assert_eq!(func[extra].def, Def::Param { block: header, index: 2 });

        let br = func.terminator(entry).expect("a terminator");
        let call = func.successors(br).nth(1).expect("the branch to the header");
        let grown = func.append_arg(call.args, extra);
        assert_eq!(func[grown].len(), 3);
    }

    #[test]
    fn a_span_rides_along_with_the_instruction() {
        let mut func = Func::new(Symbol::from_raw(0), Signature::new());
        let block = func.create_block();
        let span = Span::new(10, 20);
        let mut b = Builder::new(&mut func, block).at(span);
        let value = b.iconst(Type::int(32), 7);
        let inst = match func[value].def {
            Def::Result { inst, .. } => inst,
            Def::Param { .. } => unreachable!("a constant is not a parameter"),
        };
        assert_eq!(func.span(inst), span);
    }

    #[test]
    fn a_store_produces_nothing_and_a_load_produces_one_value() {
        let mut func = Func::new(Symbol::from_raw(0), Signature::new());
        let block = func.create_block();
        let addr = func.append_param(block, Type::PTR);
        let info = MemInfo { size: 4, align: 4, order: MemOrder::NotAtomic, tbaa: None };
        let mut b = Builder::new(&mut func, block);
        let value = b.load(Type::int(32), addr, info, Flags::NONE);
        let store = b.store(value, addr, info, Flags::VOLATILE);
        assert_eq!(func[store].results, 0);
        assert_eq!(func[store].flags, Flags::VOLATILE);
        assert_eq!(func[value].ty, Type::int(32));
    }

    #[test]
    fn a_call_produces_what_its_signature_returns() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("caller"), Signature::new());
        let sig = func.add_signature(
            Signature::new().with_params(&[Type::int(32)]).with_returns(&[Type::int(64)]),
        );
        let block = func.create_block();
        let arg = func.append_param(block, Type::int(32));
        let callee = names.intern("callee");
        let mut b = Builder::new(&mut func, block);
        let call = b.call(callee, sig, &[arg]);
        assert_eq!(func[call].results, 1);
        let value = func[call].first_result.expect("a result");
        assert_eq!(func[value].ty, Type::int(64));
        assert_eq!(func[call].extra, Extra::Call(Idx::new(0)));
    }

    #[test]
    fn the_counts_are_what_was_made() {
        let (func, _, _, _) = sum();
        let counts = func.counts();
        assert_eq!(counts.blocks, 3);
        assert_eq!(counts.insts, 9);
        // Four block parameters and five instruction results, which is the two constants, the
        // two additions and the two comparisons less the branches, which produce nothing.
        assert_eq!(counts.values, 4 + 6);
    }

    #[test]
    #[should_panic(expected = "the instruction is in a block")]
    fn appending_an_instruction_twice_is_refused() {
        let (mut func, entry, _, _) = sum();
        let first = func.insts(entry).next().expect("an instruction");
        func.append_inst(entry, first);
    }

    #[test]
    #[should_panic(expected = "the instruction is not in a block")]
    fn removing_an_instruction_twice_is_refused() {
        let (mut func, entry, _, _) = sum();
        let first = func.insts(entry).next().expect("an instruction");
        func.remove_inst(first);
        func.remove_inst(first);
    }
}
