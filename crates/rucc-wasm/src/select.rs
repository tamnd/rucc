//! One function of IR, translated into the body of one wasm function.
//!
//! The translation walks the dominator tree as Ramsey's algorithm says, and writes each block's
//! instructions where the walk reaches the block. See the crate documentation for the
//! conventions that it keeps: a local for each value, constants written where they are used, and
//! narrow integers with undefined upper bits.

use rucc_base::hash::Map;
use rucc_base::rules::Piece;
use rucc_ir::term::{PLAIN, Term, Terms};
use rucc_ir::{
    Abi, Block, Def, Extra, FloatPred, Func, FuncId, Inst, IntPred, Opcode, RmwOp, Type, Value,
};
use rucc_object::wasm::{FuncType, Function, RelocKind, ValType};
use rucc_target::wasm::Feature;

use crate::emit::{self, Code};
use crate::irreducible::Node;
use crate::rules;
use crate::structure::Shape;
use crate::{Notes, Unit, functype, is_pair, label_numbers, valtype};

mod builtin;
mod pair;

type Result<T> = std::result::Result<T, String>;

/// The frames on the way out of the code being written, innermost last. A branch names a frame
/// by how far it is from the innermost one.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ctx {
    /// A `loop` whose start is the block at this position. A branch to it goes back to the start.
    Loop(usize),
    /// A `block` that the code of the block at this position follows. A branch to it goes there.
    Follow(usize),
    /// An `if` or the `block` of one case of a switch, which nothing branches to by name.
    Other,
}

/// The stack frame of one function in linear memory.
#[derive(Default)]
struct Frame {
    /// The bytes it takes, a multiple of 16.
    size: u32,
    /// The alignment of its lowest address, at least 16.
    align: u32,
    /// The offset of each fixed `alloca` from the frame pointer.
    slots: Map<Inst, u32>,
    /// Whether the function moves the stack pointer after its prologue, with a dynamic `alloca`
    /// or with `stackrestore`.
    dynamic: bool,
    /// The local that holds the frame pointer, which is the lowest address of the frame.
    fp: Option<u32>,
    /// The local that holds the stack pointer as the function found it.
    base: Option<u32>,
    /// The offset of the buffer that a call to the runtime writes a pair to, when the function
    /// has a pair. It is 32 bytes, with the pair at the start and the overflow flag of
    /// `__muloti4` at 16.
    scratch: Option<u32>,
}

/// The bytes and the alignment of a value in the buffer of the extra arguments of a variadic
/// call. Each one takes at least 4 bytes, and the alignment is the natural one, which is 16 for a
/// pair, as clang's `va_arg` reads it.
fn va_slot(ty: Type) -> Result<(u32, u32)> {
    if is_pair(ty) {
        return Ok((16, 16));
    }
    match valtype(ty)? {
        ValType::I32 | ValType::F32 => Ok((4, 4)),
        ValType::I64 | ValType::F64 => Ok((8, 8)),
    }
}

/// The offset of each extra argument in the buffer, and the size of the buffer.
fn va_layout(types: impl Iterator<Item = Type>) -> Result<(Vec<u32>, u32)> {
    let mut offsets = Vec::new();
    let mut at = 0u32;
    for ty in types {
        let (size, align) = va_slot(ty)?;
        at = at.next_multiple_of(align);
        offsets.push(at);
        at += size;
    }
    Ok((offsets, at))
}

/// The opcode of a load into a value of type `ty`, and the natural alignment of the access.
fn load_op(ty: Type) -> Result<(u8, u32)> {
    if !ty.is_ptr() && ty.is_int() && ty.bits() <= 16 {
        return Ok(if ty.bits() <= 8 { (emit::I32_LOAD8_U, 1) } else { (emit::I32_LOAD16_U, 2) });
    }
    Ok(match valtype(ty)? {
        ValType::I32 => (emit::I32_LOAD, 4),
        ValType::I64 => (emit::I64_LOAD, 8),
        ValType::F32 => (emit::F32_LOAD, 4),
        ValType::F64 => (emit::F64_LOAD, 8),
    })
}

/// The opcode of a store of a value of type `ty`, and the natural alignment of the access.
fn store_op(ty: Type) -> Result<(u8, u32)> {
    if !ty.is_ptr() && ty.is_int() && ty.bits() <= 16 {
        return Ok(if ty.bits() <= 8 { (emit::I32_STORE8, 1) } else { (emit::I32_STORE16, 2) });
    }
    Ok(match valtype(ty)? {
        ValType::I32 => (emit::I32_STORE, 4),
        ValType::I64 => (emit::I64_STORE, 8),
        ValType::F32 => (emit::F32_STORE, 4),
        ValType::F64 => (emit::F64_STORE, 8),
    })
}

/// The alignment field of a memory access, which is a power of two and is never more than the
/// natural alignment of the access.
fn align_field(align: u32, natural: u32) -> u32 {
    align.clamp(1, natural).trailing_zeros()
}

/// The opcode of an integer comparison, for `i32` or for `i64`.
fn int_compare(pred: IntPred, wide: bool) -> u8 {
    let base = if wide { emit::I64_EQ } else { emit::I32_EQ };
    base + match pred {
        IntPred::Eq => 0,
        IntPred::Ne => 1,
        IntPred::Slt => 2,
        IntPred::Ult => 3,
        IntPred::Sgt => 4,
        IntPred::Ugt => 5,
        IntPred::Sle => 6,
        IntPred::Ule => 7,
        IntPred::Sge => 8,
        IntPred::Uge => 9,
    }
}

/// An `i32` arithmetic opcode, moved to its `i64` form when `wide` is set.
fn int_op(op: u8, wide: bool) -> u8 {
    if wide { op + emit::I64_FROM_I32 } else { op }
}

/// Translate the function `id`, whose symbol is `symbol`.
pub(crate) fn function(unit: &mut Unit<'_>, id: FuncId, symbol: u32) -> Result<Function> {
    let func = &unit.ir[id];
    let export = func.wasm.export.map(|name| unit.names.resolve(name).to_owned());
    let shape = Shape::of(func)?;
    let unit_notes = unit.notes.is_some();
    let ty = functype(func.signature())?;
    let params = u32::try_from(ty.params.len()).expect("fewer than 2^32 parameters");
    let mut lower = Lower {
        unit,
        func,
        shape,
        code: Code::default(),
        params,
        locals: Vec::new(),
        local: Map::default(),
        frame: Frame::default(),
        context: Vec::new(),
        labels: Vec::new(),
        numbers: label_numbers(func),
        va: func.signature().variadic.then(|| params - 1),
        returns: !ty.results.is_empty(),
        sret: func.signature().returns.iter().any(|ret| is_pair(ret.ty)),
        annotate: None,
    };
    if unit_notes {
        let (values, blocks) = rucc_ir::numbers(func);
        lower.annotate = Some(Annotate { notes: Notes::default(), values, blocks });
    }
    lower.assign()?;
    lower.labels = (0..lower.shape.dispatches).map(|_| lower.new_local(ValType::I32)).collect();
    lower.plan()?;
    lower.prologue();
    if !lower.shape.order.is_empty() {
        lower.tree(0)?;
    }
    if lower.returns {
        lower.code.op(emit::UNREACHABLE);
    }
    lower.code.op(emit::END);
    lower.name_locals();
    if let (Some(notes), Some(annotate)) = (lower.unit.notes.as_mut(), lower.annotate.take()) {
        notes.push(annotate.notes);
    }

    let mut locals: Vec<(u32, ValType)> = Vec::new();
    for &ty in &lower.locals {
        match locals.last_mut() {
            Some((count, last)) if *last == ty => *count += 1,
            _ => locals.push((1, ty)),
        }
    }
    Ok(Function { symbol, locals, code: lower.code.bytes, fixups: lower.code.fixups, export })
}

struct Lower<'u, 'a> {
    unit: &'u mut Unit<'a>,
    func: &'a Func,
    shape: Shape,
    code: Code,
    /// How many parameters the wasm function has. The locals after them are numbered from here.
    params: u32,
    /// The type of each local after the parameters.
    locals: Vec<ValType>,
    /// The local of each value that has one.
    local: Map<Value, u32>,
    frame: Frame,
    context: Vec<Ctx>,
    /// The label local of each dispatch node, which says the entry of its loop to go to.
    labels: Vec<u32>,
    /// The number of each block whose address is taken, which is the address.
    numbers: Map<Block, u32>,
    /// The parameter that holds the address of the extra arguments, in a variadic function.
    va: Option<u32>,
    returns: bool,
    /// Whether the function returns a pair, through the address in its parameter 0.
    sret: bool,
    /// The notes of the tree form, when it is asked for.
    annotate: Option<Annotate>,
}

/// The notes of the tree form for one function, and the numbers that the text of the IR gives
/// its values and blocks, which the notes use as names.
struct Annotate {
    notes: Notes,
    values: Vec<u32>,
    blocks: Vec<u32>,
}

impl Lower<'_, '_> {
    /// Write a note beside the next instruction, for the tree form. `text` gets the name of the
    /// node at `x`, which is the name of its IR block or of its dispatch node.
    fn mark(&mut self, x: usize, text: impl FnOnce(String) -> String) {
        let at = self.code.bytes.len();
        let node = self.shape.order[x];
        let Some(annotate) = self.annotate.as_mut() else { return };
        let name = match node {
            Node::Block(block) => format!("block{}", annotate.blocks[block.index()]),
            Node::Dispatch(label) => format!("dispatch{label}"),
        };
        annotate.notes.marks.push((at, text(name)));
    }

    /// Give each local that holds an IR value the name of the value, for the tree form. A pair
    /// is two locals, the low half and the high half.
    fn name_locals(&mut self) {
        let Some(annotate) = self.annotate.as_mut() else { return };
        let locals = &mut annotate.notes.locals;
        for (&value, &local) in &self.local {
            let name = format!("%{}", annotate.values[value.index()]);
            if is_pair(self.func[value].ty) {
                locals.insert(local, format!("{name}.lo"));
                locals.insert(local + 1, format!("{name}.hi"));
            } else {
                locals.insert(local, name);
            }
        }
        for (index, &local) in self.labels.iter().enumerate() {
            locals.insert(local, format!("label{index}"));
        }
        if self.sret {
            locals.insert(0, "sret".into());
        }
        if let Some(va) = self.va {
            locals.insert(va, "va".into());
        }
        if let (Some(fp), Some(base)) = (self.frame.fp, self.frame.base) {
            locals.insert(fp, "frame".into());
            locals.insert(base, "entry_sp".into());
        }
    }

    /// The blocks of the function that are reached, in the order of the structure.
    fn blocks(&self) -> Vec<Block> {
        let blocks = self.shape.order.iter().filter_map(|&node| match node {
            Node::Block(block) => Some(block),
            Node::Dispatch(_) => None,
        });
        blocks.collect()
    }

    fn new_local(&mut self, ty: ValType) -> u32 {
        let index = self.params + u32::try_from(self.locals.len()).expect("fewer locals");
        self.locals.push(ty);
        index
    }

    /// A local for a value of type `ty`. A pair takes two locals one after the other, and its
    /// local is the one of its low half.
    fn local_for(&mut self, ty: Type) -> Result<u32> {
        if is_pair(ty) {
            let low = self.new_local(ValType::I64);
            self.new_local(ValType::I64);
            return Ok(low);
        }
        Ok(self.new_local(valtype(ty)?))
    }

    /// Give each value a local. The parameters of the entry block are the parameters of the
    /// function, after the address of the return value when there is one, and a constant has
    /// none, because it is written where it is used.
    fn assign(&mut self) -> Result<()> {
        let func = self.func;
        for block in self.blocks() {
            let params = func[block].params.iter().copied().filter(|&v| !func[v].ty.is_mem());
            let mut next = u32::from(self.sret);
            for value in params {
                let local = if Some(block) == func.entry() {
                    let local = next;
                    next += if is_pair(func[value].ty) { 2 } else { 1 };
                    local
                } else {
                    self.local_for(func[value].ty)?
                };
                self.local.insert(value, local);
            }
            for inst in func.insts(block) {
                if matches!(
                    func[inst].opcode,
                    Opcode::IConst | Opcode::FConst | Opcode::GlobalAddr | Opcode::BlockAddr
                ) {
                    continue;
                }
                for value in func[inst].results() {
                    let ty = func[value].ty;
                    if ty.is_mem() || ty.is_void() {
                        continue;
                    }
                    let local = self.local_for(ty)?;
                    self.local.insert(value, local);
                }
            }
        }
        Ok(())
    }

    /// Lay out the stack frame: the buffer for the extra arguments of the variadic calls at the
    /// bottom, the buffer for the answers of the runtime above it, and the fixed `alloca` slots
    /// above that.
    fn plan(&mut self) -> Result<()> {
        let func = self.func;
        let mut va = 0u32;
        let mut scratch = false;
        let mut allocas = Vec::new();
        for block in self.blocks() {
            for inst in func.insts(block) {
                let data = &func[inst];
                scratch |=
                    pair::calls_runtime(data.opcode) && data.results().any(|v| is_pair(func[v].ty));
                match data.opcode {
                    Opcode::Alloca if self.args(inst).is_empty() => {
                        let Extra::Mem(mem) = data.extra else { continue };
                        let mem = func[mem];
                        let size = u32::try_from(mem.size)
                            .map_err(|_| "a local of 4 GiB or more does not fit".to_owned())?;
                        allocas.push((inst, size, mem.align.max(1)));
                    }
                    Opcode::Alloca | Opcode::StackRestore => self.frame.dynamic = true,
                    Opcode::Call | Opcode::CallIndirect | Opcode::TailCall => {
                        let Extra::Call(info) = data.extra else { continue };
                        let sig = &func[func[info].signature];
                        if sig.variadic {
                            let args = self.args(inst);
                            let skip = usize::from(func[info].callee.is_none());
                            let fixed = sig.params.iter().filter(|p| !p.ty.is_mem()).count();
                            let extra = args[skip + fixed..].iter().map(|&v| func[v].ty);
                            va = va.max(va_layout(extra)?.1);
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut at = va.next_multiple_of(16);
        if scratch {
            self.frame.scratch = Some(at);
            at += 32;
        }
        let mut align = 16;
        for (inst, size, a) in allocas {
            at = at.next_multiple_of(a);
            self.frame.slots.insert(inst, at);
            at += size;
            align = align.max(a);
        }
        self.frame.size = at.next_multiple_of(16);
        self.frame.align = align;
        if self.frame.size > 0 || self.frame.dynamic {
            self.frame.fp = Some(self.new_local(ValType::I32));
            self.frame.base = Some(self.new_local(ValType::I32));
        }
        Ok(())
    }

    /// Move the stack pointer down past the frame, and keep where it was so that each return can
    /// put it back.
    fn prologue(&mut self) {
        let (Some(fp), Some(base)) = (self.frame.fp, self.frame.base) else { return };
        let sp = self.unit.stack_pointer();
        self.code.global_get(sp);
        self.code.local_tee(base);
        self.code.i32_const(self.frame.size as i32);
        self.code.op(emit::I32_SUB);
        if self.frame.align > 16 {
            self.code.i32_const(-(self.frame.align as i32));
            self.code.op(emit::I32_AND);
        }
        self.code.local_tee(fp);
        self.code.global_set(sp);
    }

    fn epilogue(&mut self) {
        if let Some(base) = self.frame.base {
            let sp = self.unit.stack_pointer();
            self.code.local_get(base);
            self.code.global_set(sp);
        }
    }

    // The structure, after Ramsey. `tree` is doTree, `within` is nodeWithin and `branch` is
    // doBranch, and the names of the frames are his.

    fn tree(&mut self, x: usize) -> Result<()> {
        let mut merges: Vec<usize> =
            self.shape.children[x].iter().copied().filter(|&c| self.shape.merge[c]).collect();
        merges.reverse();
        if self.shape.loop_header[x] {
            self.mark(x, |name| format!("loop of {name}"));
            self.code.open(emit::LOOP, None);
            self.context.push(Ctx::Loop(x));
            self.within(x, &merges)?;
            self.context.pop();
            self.code.op(emit::END);
            Ok(())
        } else {
            self.within(x, &merges)
        }
    }

    fn within(&mut self, x: usize, merges: &[usize]) -> Result<()> {
        let Some((&y, rest)) = merges.split_first() else { return self.body(x) };
        self.mark(y, |name| format!("{name} follows"));
        self.code.open(emit::BLOCK, None);
        self.context.push(Ctx::Follow(y));
        self.within(x, rest)?;
        self.context.pop();
        self.code.op(emit::END);
        self.tree(y)
    }

    fn depth(&self, frame: Ctx) -> u32 {
        let at = self.context.iter().rposition(|&c| c == frame).expect("the frame is open");
        u32::try_from(self.context.len() - 1 - at).expect("fewer than 2^32 frames")
    }

    /// The edge `index` of the node at `x`, which for a block is the target `index` of its
    /// terminator. The edge writes the parameters of the block that it goes to, and the label of
    /// each dispatch node that it passes, and then it goes.
    fn branch(&mut self, x: usize, index: usize) -> Result<()> {
        let func = self.func;
        if let Node::Block(block) = self.shape.order[x] {
            let term = func.terminator(block).ok_or("a block has no terminator")?;
            let call = func.successors(term).nth(index).ok_or("an edge with no target")?;
            let pairs: Vec<(Value, Value)> = func[call.args]
                .iter()
                .copied()
                .zip(func[call.block].params.iter().copied())
                .filter(|&(arg, param)| !func[param].ty.is_mem() && arg != param)
                .collect();
            for &(arg, _) in &pairs {
                self.push(arg)?;
            }
            for &(_, param) in pairs.iter().rev() {
                self.set(param);
            }
        }
        let edge = self.shape.edges[x][index].clone();
        for &(label, which) in &edge.labels {
            self.code.i32_const(which as i32);
            self.code.local_set(self.labels[label]);
        }
        let to = edge.to;
        if self.shape.is_backward(x, to) {
            self.mark(to, |name| format!("back to {name}"));
            let depth = self.depth(Ctx::Loop(to));
            self.code.br(depth);
            Ok(())
        } else if self.shape.merge[to] {
            self.mark(to, |name| format!("out to {name}"));
            let depth = self.depth(Ctx::Follow(to));
            self.code.br(depth);
            Ok(())
        } else {
            self.tree(to)
        }
    }

    fn body(&mut self, x: usize) -> Result<()> {
        self.mark(x, |name| name);
        let func = self.func;
        let block = match self.shape.order[x] {
            Node::Block(block) => block,
            Node::Dispatch(label) => return self.dispatch(x, label),
        };
        let Some(term) = func.terminator(block) else {
            return Err("a block has no terminator".into());
        };
        let caught = self.caught(term);
        for inst in func.insts(block) {
            if inst != term && caught.is_none_or(|(call, unwound)| inst != call && inst != unwound)
            {
                self.inst(inst)?;
            }
        }
        match func[term].opcode {
            Opcode::Jump => self.branch(x, 0),
            Opcode::BrIf if caught.is_some() => {
                let (call, _) = caught.expect("checked just above");
                self.catch(x, term, call)
            }
            Opcode::BrIf => {
                let cond = self.args(term)[0];
                self.push_z(cond)?;
                self.code.open(emit::IF, None);
                self.context.push(Ctx::Other);
                self.branch(x, 0)?;
                self.code.op(emit::ELSE);
                self.branch(x, 1)?;
                self.context.pop();
                self.code.op(emit::END);
                Ok(())
            }
            Opcode::Switch => self.switch(x, term),
            Opcode::Return if self.sret => {
                self.epilogue();
                let value = self.args(term)[0];
                self.store_pair(value, |s| {
                    s.code.local_get(0);
                    Ok(())
                })?;
                self.code.op(emit::RETURN);
                Ok(())
            }
            Opcode::Return => {
                self.epilogue();
                let sig = func.signature();
                let values = self.args(term);
                let abis = sig.returns.iter().filter(|p| !p.ty.is_mem() && !p.ty.is_void());
                for (&value, ret) in values.iter().zip(abis) {
                    self.push_abi(value, ret.abi)?;
                }
                self.code.op(emit::RETURN);
                Ok(())
            }
            Opcode::Unreachable => {
                self.code.op(emit::UNREACHABLE);
                Ok(())
            }
            Opcode::TailCall => self.call(term, true),
            Opcode::IndirectBr => self.indirect(x, term),
            other => Err(format!("the terminator {} is not translated yet", other.name())),
        }
    }

    /// The call and the `unwound` before the `br_if` `term`, when the branch is the edge that a
    /// `longjmp` out of the call takes. See `sjlj.rs`.
    fn caught(&self, term: Inst) -> Option<(Inst, Inst)> {
        let func = self.func;
        if func[term].opcode != Opcode::BrIf {
            return None;
        }
        let (unwound, _) = self.def(self.args(term)[0])?;
        if func[unwound].opcode != Opcode::Unwound || func.next_inst(unwound) != Some(term) {
            return None;
        }
        let call = func.prev_inst(unwound)?;
        matches!(func[call].opcode, Opcode::Call | Opcode::CallIndirect).then_some((call, unwound))
    }

    /// The call `call` in a `try_table` that catches the exception of a `longjmp`, and the edges
    /// of the `br_if` `term` after it. Edge 0 goes to the dispatch, whose `landing` gets the
    /// address that the exception carries, and edge 1 goes on after the call:
    ///
    /// ```text
    /// block
    ///   block (result i32)
    ///     try_table (catch __c_longjmp 0)
    ///       call ...
    ///       br 2
    ///     end
    ///     unreachable
    ///   end
    ///   local.set landing
    ///   (edge 0)
    /// end
    /// (edge 1)
    /// ```
    ///
    /// This is the code of clang for a call after a `setjmp`.
    fn catch(&mut self, x: usize, term: Inst, call: Inst) -> Result<()> {
        let func = self.func;
        let pad = func.successors(term).next().ok_or("a br_if with no target")?.block;
        let landing = func
            .insts(pad)
            .next()
            .filter(|&first| func[first].opcode == Opcode::Landing)
            .ok_or("the target of an unwind edge does not start with landing")?;
        let landing = self.results(landing)[0];
        let local = *self.local.get(&landing).ok_or("a landing with no local")?;
        let tag = self.unit.longjmp_tag();
        self.code.open(emit::BLOCK, None);
        self.context.push(Ctx::Other);
        self.code.open(emit::BLOCK, Some(ValType::I32));
        self.context.push(Ctx::Other);
        self.code.try_table(tag, 0);
        self.context.push(Ctx::Other);
        self.inst(call)?;
        self.code.br(2);
        self.context.pop();
        self.code.op(emit::END);
        self.code.op(emit::UNREACHABLE);
        self.context.pop();
        self.code.op(emit::END);
        self.code.local_set(local);
        self.branch(x, 0)?;
        self.context.pop();
        self.code.op(emit::END);
        self.branch(x, 1)
    }

    /// A switch, as one `block` for each distinct target around a `br_table` when the cases are
    /// dense and around a run of compares when they are not.
    fn switch(&mut self, x: usize, term: Inst) -> Result<()> {
        let func = self.func;
        let Extra::Switch(info) = func[term].extra else {
            return Err("a switch without its cases".into());
        };
        let info = func[info];
        let calls = &func[info.targets];
        let value = self.args(term)[0];
        let ty = func[value].ty;
        let wide = valtype(ty)? == ValType::I64;

        // The distinct targets, as the index of the first edge to each. Two cases that go to one
        // block with the same arguments share it.
        let mut targets: Vec<usize> = Vec::new();
        let mut which = Vec::with_capacity(calls.len());
        for (index, call) in calls.iter().enumerate() {
            let same = targets.iter().position(|&t| {
                calls[t].block == call.block && func[calls[t].args] == func[call.args]
            });
            which.push(same.unwrap_or_else(|| {
                targets.push(index);
                targets.len() - 1
            }));
        }
        let default = which[0] as u32;
        let cases: Vec<(i128, u32)> = func[info.cases]
            .iter()
            .zip(&which[1..])
            .map(|(imm, &t)| (imm.signed(ty), t as u32))
            .collect();

        for _ in &targets {
            self.code.open(emit::BLOCK, None);
            self.context.push(Ctx::Other);
        }
        let (min, max) =
            cases.iter().fold((i128::MAX, i128::MIN), |(lo, hi), &(c, _)| (lo.min(c), hi.max(c)));
        let span = max - min;
        if !cases.is_empty() && span < 1 << 16 && span < 4 * cases.len() as i128 + 8 {
            let mut table = vec![default; usize::try_from(span + 1).expect("a small span")];
            for &(c, t) in &cases {
                table[usize::try_from(c - min).expect("in the span")] = t;
            }
            if wide {
                let index = self.new_local(ValType::I64);
                self.push(value)?;
                self.code.i64_const(i64::try_from(min).expect("an i64 case"));
                self.code.op(int_op(emit::I32_SUB, true));
                self.code.local_tee(index);
                self.code.i64_const(i64::try_from(span).expect("a small span"));
                self.code.op(emit::I64_GT_U);
                self.code.br_if(default);
                self.code.local_get(index);
                self.code.op(emit::I32_WRAP_I64);
            } else {
                self.push_s(value)?;
                if min != 0 {
                    self.code.i32_const(i32::try_from(min).expect("an i32 case"));
                    self.code.op(emit::I32_SUB);
                }
            }
            self.code.br_table(&table, default);
        } else {
            for &(c, t) in &cases {
                if wide {
                    self.push(value)?;
                    self.code.i64_const(i64::try_from(c).expect("an i64 case"));
                    self.code.op(emit::I64_EQ);
                } else {
                    self.push_s(value)?;
                    self.code.i32_const(i32::try_from(c).expect("an i32 case"));
                    self.code.op(emit::I32_EQ);
                }
                self.code.br_if(t);
            }
            self.code.br(default);
        }
        for target in targets {
            self.context.pop();
            self.code.op(emit::END);
            self.branch(x, target)?;
        }
        Ok(())
    }

    /// A computed `goto`, as one `block` for each distinct target around a `br_table` on the
    /// number of the label, which is the address that [`label_numbers`] gives the label. The
    /// numbers are dense, so the table is short. An address that is not one of the targets is a
    /// jump that the program said it does not make, and it goes to an `unreachable`, which section
    /// 7.5 of the WebAssembly notes asks for.
    ///
    /// The `br_table` is at each `goto`, where the notes put one dispatch block for the function.
    /// Each `goto` passes its own arguments to each target, so one dispatch block would need a
    /// parameter for each parameter of each target, and each `goto` would write all of them.
    fn indirect(&mut self, x: usize, term: Inst) -> Result<()> {
        let func = self.func;
        let address = self.args(term)[0];
        // The distinct targets, as the index of the first edge to each, and the depth that each
        // number goes to. The address names the block and not the arguments, so a second edge to
        // the same block is never taken. The depth is one more than the index, because the
        // innermost `block` is the one that ends in `unreachable`.
        let mut targets: Vec<usize> = Vec::new();
        let mut cases: Vec<(u32, u32)> = Vec::new();
        for (index, call) in func.successors(term).enumerate() {
            let number = self.numbers[&call.block];
            if cases.iter().all(|&(n, _)| n != number) {
                targets.push(index);
                cases.push((number, targets.len() as u32));
            }
        }
        for _ in 0..=targets.len() {
            self.code.open(emit::BLOCK, None);
            self.context.push(Ctx::Other);
        }
        let min = cases.iter().map(|&(n, _)| n).min().unwrap_or(1);
        let max = cases.iter().map(|&(n, _)| n).max().unwrap_or(1);
        let mut table = vec![0; (max - min + 1) as usize];
        for &(n, depth) in &cases {
            table[(n - min) as usize] = depth;
        }
        self.push(address)?;
        self.code.i32_const(min as i32);
        self.code.op(emit::I32_SUB);
        self.code.br_table(&table, 0);
        self.context.pop();
        self.code.op(emit::END);
        self.code.op(emit::UNREACHABLE);
        for target in targets {
            self.context.pop();
            self.code.op(emit::END);
            self.branch(x, target)?;
        }
        Ok(())
    }

    /// A dispatch node, as one `block` for each entry of its loop around a `br_table` on its
    /// label.
    fn dispatch(&mut self, x: usize, label: usize) -> Result<()> {
        let entries = self.shape.edges[x].len();
        for _ in 0..entries {
            self.code.open(emit::BLOCK, None);
            self.context.push(Ctx::Other);
        }
        let last = u32::try_from(entries - 1).expect("fewer than 2^32 entries");
        let table: Vec<u32> = (0..=last).collect();
        self.code.local_get(self.labels[label]);
        self.code.br_table(&table, last);
        for index in 0..entries {
            self.context.pop();
            self.code.op(emit::END);
            self.branch(x, index)?;
        }
        Ok(())
    }

    // Values.

    fn args(&self, inst: Inst) -> Vec<Value> {
        let func = self.func;
        func[func[inst].args].iter().copied().filter(|&v| !func[v].ty.is_mem()).collect()
    }

    fn results(&self, inst: Inst) -> Vec<Value> {
        let func = self.func;
        func[inst].results().filter(|&v| !func[v].ty.is_mem() && !func[v].ty.is_void()).collect()
    }

    fn ty(&self, value: Value) -> Type {
        self.func[value].ty
    }

    fn wide(&self, value: Value) -> bool {
        valtype(self.ty(value)) == Ok(ValType::I64)
    }

    /// The bits of a narrow integer, or nothing for a value of 32 bits or more.
    fn narrow(&self, value: Value) -> Option<u32> {
        let ty = self.ty(value);
        (ty.is_int() && !ty.is_ptr() && ty.bits() < 32).then(|| ty.bits())
    }

    /// Take a value off the operand stack into its local, or into its two locals for a pair,
    /// whose high half is on top.
    fn set(&mut self, value: Value) {
        let local = self.local[&value];
        if is_pair(self.ty(value)) {
            self.code.local_set(local + 1);
        }
        self.code.local_set(local);
    }

    fn def(&self, value: Value) -> Option<(Inst, usize)> {
        match self.func[value].def {
            Def::Result { inst, index } => Some((inst, usize::from(index))),
            Def::Param { .. } => None,
        }
    }

    /// Whether the upper bits of a narrow value are known to be zero.
    fn clean(&self, value: Value) -> bool {
        let Some((inst, index)) = self.def(value) else { return false };
        match self.func[inst].opcode {
            Opcode::ICmp
            | Opcode::FCmp
            | Opcode::IConst
            | Opcode::Load
            | Opcode::AtomicLoad
            | Opcode::AtomicRmw
            | Opcode::Cmpxchg
            | Opcode::ZExt => true,
            Opcode::SAddOverflow
            | Opcode::UAddOverflow
            | Opcode::SSubOverflow
            | Opcode::USubOverflow
            | Opcode::SMulOverflow
            | Opcode::UMulOverflow => index == 1,
            _ => false,
        }
    }

    /// Put a value on the operand stack. A constant is written here, and anything else is read
    /// from its local. A pair is two values, the low half first.
    fn push(&mut self, value: Value) -> Result<()> {
        if is_pair(self.ty(value)) {
            self.push_half(value, false)?;
            return self.push_half(value, true);
        }
        if let Some((inst, _)) = self.def(value) {
            let data = &self.func[inst];
            match (data.opcode, data.extra) {
                (Opcode::IConst, Extra::Imm(imm)) => {
                    let bits = self.func[imm].bits();
                    if self.wide(value) {
                        self.code.i64_const(bits as u64 as i64);
                    } else {
                        self.code.i32_const(bits as u32 as i32);
                    }
                    return Ok(());
                }
                (Opcode::FConst, Extra::Imm(imm)) => {
                    let bits = self.func[imm].bits();
                    match valtype(self.ty(value))? {
                        ValType::F32 => self.code.f32_const(bits as u32),
                        _ => self.code.f64_const(bits as u64),
                    }
                    return Ok(());
                }
                (Opcode::GlobalAddr, Extra::Symbol(symbol)) => {
                    let (function, target) = self.unit.address(symbol)?;
                    let kind = if function {
                        RelocKind::TableIndexSleb
                    } else {
                        RelocKind::MemoryAddrSleb
                    };
                    self.code.address(kind, target, 0);
                    return Ok(());
                }
                (Opcode::BlockAddr, _) => {
                    let call =
                        self.func.successors(inst).next().ok_or("a block_addr names no block")?;
                    self.code.i32_const(self.numbers[&call.block] as i32);
                    return Ok(());
                }
                _ => {}
            }
        }
        let Some(&local) = self.local.get(&value) else {
            return Err(format!("a value of type {} has no local", self.ty(value)));
        };
        self.code.local_get(local);
        Ok(())
    }

    /// Push a value sign extended to the width of its value type.
    fn push_s(&mut self, value: Value) -> Result<()> {
        self.push(value)?;
        if let Some(bits) = self.narrow(value) {
            self.sign_extend(bits);
        }
        Ok(())
    }

    /// Push a value zero extended to the width of its value type.
    fn push_z(&mut self, value: Value) -> Result<()> {
        self.push(value)?;
        if let (Some(bits), false) = (self.narrow(value), self.clean(value)) {
            self.code.i32_const(((1u32 << bits) - 1) as i32);
            self.code.op(emit::I32_AND);
        }
        Ok(())
    }

    /// Push a value the way the ABI says it travels, which is extended when it is narrow.
    fn push_abi(&mut self, value: Value, abi: Abi) -> Result<()> {
        match abi {
            Abi::Sext => self.push_s(value),
            Abi::Zext => self.push_z(value),
            Abi::ByVal { .. } => {
                Err("an argument that travels by value in memory is not translated yet".into())
            }
            _ if self.narrow(value) == Some(1) => self.push_z(value),
            _ => self.push(value),
        }
    }

    /// Sign extend the `i32` on the stack from its low `bits`.
    fn sign_extend(&mut self, bits: u32) {
        let signext = self.unit.features.has(Feature::SignExt);
        match bits {
            8 if signext => self.code.op(emit::I32_EXTEND8_S),
            16 if signext => self.code.op(emit::I32_EXTEND16_S),
            _ => {
                let shift = (32 - bits) as i32;
                self.code.i32_const(shift);
                self.code.op(emit::I32_SHL);
                self.code.i32_const(shift);
                self.code.op(emit::I32_SHR_S);
            }
        }
    }

    /// Change the integer on the stack from `i32` to `i64`, or back, as `from` and `to` say.
    fn resize(&mut self, from_wide: bool, to_wide: bool, signed: bool) {
        match (from_wide, to_wide) {
            (true, false) => self.code.op(emit::I32_WRAP_I64),
            (false, true) if signed => self.code.op(emit::I64_EXTEND_I32_S),
            (false, true) => self.code.op(emit::I64_EXTEND_I32_U),
            _ => {}
        }
    }

    /// Push an integer of either width as an `i32`, which is what an address and a length are.
    fn push_i32(&mut self, value: Value) -> Result<()> {
        if self.wide(value) {
            self.push(value)?;
            self.code.op(emit::I32_WRAP_I64);
            Ok(())
        } else {
            self.push_z(value)
        }
    }

    fn mem_info(&self, inst: Inst) -> Option<rucc_ir::MemInfo> {
        match self.func[inst].extra {
            Extra::Mem(mem) | Extra::Rmw(_, mem) => Some(self.func[mem]),
            _ => None,
        }
    }

    fn constant(&self, value: Value) -> Option<u128> {
        let (inst, _) = self.def(value)?;
        match (self.func[inst].opcode, self.func[inst].extra) {
            (Opcode::IConst, Extra::Imm(imm)) => Some(self.func[imm].bits()),
            _ => None,
        }
    }

    fn frame_pointer(&self) -> u32 {
        self.frame.fp.expect("a function with a slot in its frame has a frame pointer")
    }

    // Instructions.

    #[allow(clippy::too_many_lines)]
    fn inst(&mut self, inst: Inst) -> Result<()> {
        let func = self.func;
        let data = &func[inst];
        let args = self.args(inst);
        let results = self.results(inst);
        let call = matches!(data.opcode, Opcode::Call | Opcode::CallIndirect);
        if !call && args.iter().chain(&results).any(|&v| is_pair(self.ty(v))) {
            return self.pair(inst, &args, &results);
        }
        if rules::tried(data.opcode) && self.by_rule(inst)? {
            return Ok(());
        }
        let arg = |i: usize| args[i];
        match data.opcode {
            Opcode::IConst | Opcode::FConst | Opcode::GlobalAddr | Opcode::BlockAddr => {}
            Opcode::Add
            | Opcode::Sub
            | Opcode::Mul
            | Opcode::And
            | Opcode::Or
            | Opcode::Xor
            | Opcode::Shl => {
                let wide = self.wide(results[0]);
                let op = match data.opcode {
                    Opcode::Add => emit::I32_ADD,
                    Opcode::Sub => emit::I32_SUB,
                    Opcode::Mul => emit::I32_MUL,
                    Opcode::And => emit::I32_AND,
                    Opcode::Or => emit::I32_OR,
                    Opcode::Xor => emit::I32_XOR,
                    _ => emit::I32_SHL,
                };
                self.push(arg(0))?;
                if data.opcode == Opcode::Shl {
                    self.shift_count(arg(1), wide)?;
                } else {
                    self.push(arg(1))?;
                }
                self.code.op(int_op(op, wide));
                self.set(results[0]);
            }
            Opcode::SDiv | Opcode::SRem | Opcode::AShr => {
                let wide = self.wide(results[0]);
                self.push_s(arg(0))?;
                let op = match data.opcode {
                    Opcode::SDiv => emit::I32_DIV_S,
                    Opcode::SRem => emit::I32_REM_S,
                    _ => emit::I32_SHR_S,
                };
                if data.opcode == Opcode::AShr {
                    self.shift_count(arg(1), wide)?;
                } else {
                    self.push_s(arg(1))?;
                }
                self.code.op(int_op(op, wide));
                self.set(results[0]);
            }
            Opcode::UDiv | Opcode::URem | Opcode::LShr => {
                let wide = self.wide(results[0]);
                self.push_z(arg(0))?;
                let op = match data.opcode {
                    Opcode::UDiv => emit::I32_DIV_U,
                    Opcode::URem => emit::I32_REM_U,
                    _ => emit::I32_SHR_U,
                };
                if data.opcode == Opcode::LShr {
                    self.shift_count(arg(1), wide)?;
                } else {
                    self.push_z(arg(1))?;
                }
                self.code.op(int_op(op, wide));
                self.set(results[0]);
            }
            Opcode::UMulHigh | Opcode::SMulHigh => {
                if self.wide(results[0]) {
                    return Err("the high half of a 64-bit product is not translated yet".into());
                }
                let signed = data.opcode == Opcode::SMulHigh;
                let bits = self.ty(results[0]).bits();
                for &a in &args[..2] {
                    if signed {
                        self.push_s(a)?
                    } else {
                        self.push_z(a)?
                    }
                    self.resize(false, true, signed);
                }
                self.code.op(int_op(emit::I32_MUL, true));
                self.code.i64_const(i64::from(bits));
                self.code.op(int_op(if signed { emit::I32_SHR_S } else { emit::I32_SHR_U }, true));
                self.code.op(emit::I32_WRAP_I64);
                self.set(results[0]);
            }
            Opcode::FAdd | Opcode::FSub | Opcode::FMul | Opcode::FDiv => {
                let base = self.float_base(results[0])?;
                self.push(arg(0))?;
                self.push(arg(1))?;
                let offset = match data.opcode {
                    Opcode::FAdd => 0,
                    Opcode::FSub => 1,
                    Opcode::FMul => 2,
                    _ => 3,
                };
                self.code.op(base + offset);
                self.set(results[0]);
            }
            Opcode::FNeg => {
                let neg = match valtype(self.ty(results[0]))? {
                    ValType::F32 => emit::F32_NEG,
                    _ => emit::F64_NEG,
                };
                self.push(arg(0))?;
                self.code.op(neg);
                self.set(results[0]);
            }
            Opcode::FRem | Opcode::Fma => {
                let vt = valtype(self.ty(results[0]))?;
                let single = vt == ValType::F32;
                let name = match (data.opcode, single) {
                    (Opcode::FRem, true) => "fmodf",
                    (Opcode::FRem, false) => "fmod",
                    (_, true) => "fmaf",
                    (_, false) => "fma",
                };
                let ty = FuncType { params: vec![vt; args.len()], results: vec![vt] };
                let (symbol, _) = self.unit.libcall(name, ty);
                for &a in &args {
                    self.push(a)?;
                }
                self.code.call(symbol, false);
                self.set(results[0]);
            }
            Opcode::ICmp => {
                let Extra::IntPred(pred) = data.extra else { return Err("icmp".into()) };
                let wide = self.wide(arg(0));
                for &a in &args[..2] {
                    if pred.is_signed() { self.push_s(a)? } else { self.push_z(a)? }
                }
                self.code.op(int_compare(pred, wide));
                self.set(results[0]);
            }
            Opcode::FCmp => {
                let Extra::FloatPred(pred) = data.extra else { return Err("fcmp".into()) };
                self.fcmp(pred, arg(0), arg(1))?;
                self.set(results[0]);
            }
            Opcode::Select => {
                self.push(arg(1))?;
                self.push(arg(2))?;
                self.push_z(arg(0))?;
                self.code.op(emit::SELECT);
                self.set(results[0]);
            }
            Opcode::Trunc => {
                self.push(arg(0))?;
                self.resize(self.wide(arg(0)), self.wide(results[0]), false);
                self.set(results[0]);
            }
            Opcode::SExt | Opcode::ZExt => {
                let signed = data.opcode == Opcode::SExt;
                if signed {
                    self.push_s(arg(0))?
                } else {
                    self.push_z(arg(0))?
                }
                self.resize(self.wide(arg(0)), self.wide(results[0]), signed);
                self.set(results[0]);
            }
            Opcode::FPTrunc | Opcode::FPExt | Opcode::Bitcast => {
                let from = valtype(self.ty(arg(0)))?;
                let to = valtype(self.ty(results[0]))?;
                self.push(arg(0))?;
                match (from, to) {
                    (ValType::F64, ValType::F32) => self.code.op(emit::F32_DEMOTE_F64),
                    (ValType::F32, ValType::F64) => self.code.op(emit::F64_PROMOTE_F32),
                    (ValType::F32, ValType::I32) => self.code.op(emit::I32_REINTERPRET_F32),
                    (ValType::F64, ValType::I64) => self.code.op(emit::I64_REINTERPRET_F64),
                    (ValType::I32, ValType::F32) => self.code.op(emit::F32_REINTERPRET_I32),
                    (ValType::I64, ValType::F64) => self.code.op(emit::F64_REINTERPRET_I64),
                    (a, b) if a == b => {}
                    (a, b) => return Err(format!("a conversion from {a} to {b}")),
                }
                self.set(results[0]);
            }
            Opcode::FPToSI | Opcode::FPToUI => {
                let from = valtype(self.ty(arg(0)))?;
                let wide = self.wide(results[0]);
                let unsigned = u8::from(data.opcode == Opcode::FPToUI);
                let double = u8::from(from == ValType::F64);
                self.push(arg(0))?;
                if self.unit.features.has(Feature::NontrappingFptoint) {
                    let op = u32::from(u8::from(wide) * 4 + double * 2 + unsigned);
                    self.code.prefixed(op);
                } else {
                    let base: u8 = if wide { 0xae } else { 0xa8 };
                    self.code.op(base + double * 2 + unsigned);
                }
                self.set(results[0]);
            }
            Opcode::SIToFP | Opcode::UIToFP => {
                let signed = data.opcode == Opcode::SIToFP;
                let to = valtype(self.ty(results[0]))?;
                let wide = self.wide(arg(0));
                if signed {
                    self.push_s(arg(0))?
                } else {
                    self.push_z(arg(0))?
                }
                let base: u8 = if to == ValType::F32 { 0xb2 } else { 0xb7 };
                self.code.op(base + u8::from(wide) * 2 + u8::from(!signed));
                self.set(results[0]);
            }
            Opcode::PtrToInt | Opcode::IntToPtr => {
                self.push_z(arg(0))?;
                self.resize(self.wide(arg(0)), self.wide(results[0]), false);
                self.set(results[0]);
            }
            Opcode::PtrAdd => {
                self.push(arg(0))?;
                if self.wide(arg(1)) {
                    self.push(arg(1))?;
                    self.code.op(emit::I32_WRAP_I64);
                } else {
                    self.push_s(arg(1))?;
                }
                self.code.op(emit::I32_ADD);
                self.set(results[0]);
            }
            Opcode::Load | Opcode::AtomicLoad => {
                let ty = self.ty(results[0]);
                let (op, natural) = load_op(ty)?;
                let align = self.mem_info(inst).map_or(natural, |m| m.align);
                self.push(arg(0))?;
                self.code.mem(op, align_field(align, natural), 0);
                self.set(results[0]);
            }
            Opcode::Store | Opcode::AtomicStore => {
                let ty = self.ty(arg(0));
                let (op, natural) = store_op(ty)?;
                let align = self.mem_info(inst).map_or(natural, |m| m.align);
                self.push(arg(1))?;
                if self.narrow(arg(0)) == Some(1) {
                    self.push_z(arg(0))?
                } else {
                    self.push(arg(0))?
                }
                self.code.mem(op, align_field(align, natural), 0);
            }
            Opcode::Alloca => {
                if let Some(&offset) = self.frame.slots.get(&inst) {
                    let fp = self.frame_pointer();
                    self.code.local_get(fp);
                    if offset != 0 {
                        self.code.i32_const(offset as i32);
                        self.code.op(emit::I32_ADD);
                    }
                } else {
                    let align = self.mem_info(inst).map_or(16, |m| m.align).max(16);
                    let sp = self.unit.stack_pointer();
                    let at = self.local[&results[0]];
                    self.code.global_get(sp);
                    self.push_i32(arg(0))?;
                    self.code.op(emit::I32_SUB);
                    self.code.i32_const(-(align as i32));
                    self.code.op(emit::I32_AND);
                    self.code.local_tee(at);
                    self.code.global_set(sp);
                    return Ok(());
                }
                self.set(results[0]);
            }
            Opcode::StackSave => {
                let sp = self.unit.stack_pointer();
                self.code.global_get(sp);
                self.set(results[0]);
            }
            Opcode::StackRestore => {
                let sp = self.unit.stack_pointer();
                self.push(arg(0))?;
                self.code.global_set(sp);
            }
            Opcode::Memcpy | Opcode::Memmove | Opcode::Memset => self.bulk(inst, &args)?,
            Opcode::AtomicRmw => self.rmw(inst, &args, &results)?,
            Opcode::Cmpxchg => self.cmpxchg(inst, &args, &results)?,
            Opcode::Call | Opcode::CallIndirect => self.call(inst, false)?,
            Opcode::SAddOverflow
            | Opcode::UAddOverflow
            | Opcode::SSubOverflow
            | Opcode::USubOverflow
            | Opcode::SMulOverflow
            | Opcode::UMulOverflow => self.overflow(data.opcode, &args, inst)?,
            Opcode::Ctlz | Opcode::Cttz | Opcode::Ctpop => {
                let wide = self.wide(arg(0));
                let bits = self.narrow(arg(0));
                match (data.opcode, bits) {
                    (Opcode::Ctlz, Some(bits)) => {
                        self.push_z(arg(0))?;
                        self.code.op(emit::I32_CLZ);
                        self.code.i32_const((32 - bits) as i32);
                        self.code.op(emit::I32_SUB);
                    }
                    (Opcode::Cttz, Some(bits)) => {
                        self.push(arg(0))?;
                        self.code.i32_const((1u32 << bits) as i32);
                        self.code.op(emit::I32_OR);
                        self.code.op(emit::I32_CTZ);
                    }
                    (op, _) => {
                        self.push_z(arg(0))?;
                        let base = match op {
                            Opcode::Ctlz => emit::I32_CLZ,
                            Opcode::Cttz => emit::I32_CTZ,
                            _ => emit::I32_POPCNT,
                        };
                        self.code.op(int_op(base, wide));
                    }
                }
                self.resize(wide, self.wide(results[0]), false);
                self.set(results[0]);
            }
            Opcode::Bswap => self.bswap(arg(0), results[0])?,
            Opcode::Expect => {
                self.push(arg(0))?;
                self.set(results[0]);
            }
            Opcode::Trap | Opcode::UnreachableHint => self.code.op(emit::UNREACHABLE),
            Opcode::Fence
            | Opcode::Prefetch
            | Opcode::LifetimeEnd
            | Opcode::MemEntry
            | Opcode::VaEnd => {}
            Opcode::ObjectSize => {
                let Extra::Question(question) = data.extra else { return Err("objsize".into()) };
                let answer = if question & 2 == 0 { -1 } else { 0 };
                if self.wide(results[0]) {
                    self.code.i64_const(answer);
                } else {
                    self.code.i32_const(answer as i32);
                }
                self.set(results[0]);
            }
            Opcode::IsConstant => {
                self.code.i32_const(0);
                self.set(results[0]);
            }
            Opcode::FrameAddress => {
                if data.extra != Extra::Depth(0) {
                    return Err("the frame address of a caller is not known on wasm".into());
                }
                match self.frame.fp {
                    Some(fp) => self.code.local_get(fp),
                    None => {
                        let sp = self.unit.stack_pointer();
                        self.code.global_get(sp);
                    }
                }
                self.set(results[0]);
            }
            Opcode::VaStart => {
                let Some(va) = self.va else {
                    return Err("va_start in a function that is not variadic".into());
                };
                self.push(arg(0))?;
                self.code.local_get(va);
                self.code.mem(emit::I32_STORE, 2, 0);
            }
            Opcode::VaCopy => {
                self.push(arg(0))?;
                self.push(arg(1))?;
                self.code.mem(emit::I32_LOAD, 2, 0);
                self.code.mem(emit::I32_STORE, 2, 0);
            }
            Opcode::VaArg => {
                let ty = self.ty(results[0]);
                let (size, align) = va_slot(ty)?;
                let cursor = self.va_cursor(arg(0), align)?;
                self.va_advance(arg(0), cursor, size);
                let (op, natural) = load_op(ty)?;
                self.code.local_get(cursor);
                self.code.mem(op, align_field(align, natural), 0);
                self.set(results[0]);
            }
            Opcode::VaObject => {
                let Extra::VaObject(info) = data.extra else { return Err("va_object".into()) };
                let info = func[info];
                let mem = func[info.mem];
                let size = u32::try_from(mem.size).map_err(|_| "a huge va_arg".to_owned())?;
                if size == 0 {
                    self.push(arg(0))?;
                    self.code.mem(emit::I32_LOAD, 2, 0);
                } else if func[info.slots].is_empty() {
                    let cursor = self.va_cursor(arg(0), 4)?;
                    self.va_advance(arg(0), cursor, 4);
                    self.code.local_get(cursor);
                    self.code.mem(emit::I32_LOAD, 2, 0);
                } else {
                    let cursor = self.va_cursor(arg(0), mem.align)?;
                    self.va_advance(arg(0), cursor, size.next_multiple_of(4));
                    self.code.local_get(cursor);
                }
                self.set(results[0]);
            }
            // An `unwound` that a `br_if` reads after a call is written with the call, and one
            // with no call in front of it answers false. A `landing` gets its value from the
            // `try_table` of the call. See [`Lower::catch`].
            Opcode::Unwound => {
                self.code.i32_const(0);
                self.set(results[0]);
            }
            Opcode::Landing => {}
            Opcode::SetjmpMarker | Opcode::LongjmpMarker => {
                return Err("`__builtin_setjmp` and `__builtin_longjmp` are not translated".into());
            }
            Opcode::InlineAsm => self.barrier(inst)?,
            other => return Err(format!("the instruction {} is not translated yet", other.name())),
        }
        Ok(())
    }

    /// Inline assembly, of which rucc takes only the compiler barrier on wasm.
    ///
    /// An `asm` with a blank template, no operands, no labels and no clobber other than `memory`
    /// and `cc` only keeps the loads and stores around it on their side of it. This translation
    /// keeps each load and store where the IR has it, in order, so the barrier is no code, as with
    /// clang. `__atomic_signal_fence` is such an `asm`. clang also reads wasm instructions in the
    /// template, and rucc does not, so any other `asm` is refused.
    fn barrier(&self, inst: Inst) -> Result<()> {
        let func = self.func;
        let Extra::Asm(asm) = func[inst].extra else {
            return Err("an inline_asm instruction has no assembly".into());
        };
        let info = func[asm];
        let names = self.unit.names;
        let blank = names.resolve(info.template).trim().is_empty();
        let operands = !self.args(inst).is_empty()
            || !self.results(inst).is_empty()
            || !names.resolve(info.constraints).is_empty()
            || !func[info.targets].is_empty();
        let clobbers = names
            .resolve(info.clobbers)
            .split(',')
            .all(|clobber| matches!(clobber.trim(), "" | "memory" | "cc"));
        if blank && !operands && clobbers {
            return Ok(());
        }
        Err("inline assembly on wasm is refused, except an `asm` with an empty template, no \
             operands and no clobber other than \"memory\" and \"cc\", which is a compiler \
             barrier"
            .into())
    }

    /// Select an instruction by the rule of `rules/wasm32.rules` that matches it, if one does.
    ///
    /// Each operand is shown to the table as a value in its local, because wasm has no immediate
    /// operand to fold a constant into, and a constant is pushed where a rule uses it. Pointers are
    /// shown at 32 bits.
    fn by_rule(&mut self, inst: Inst) -> Result<bool> {
        let &[result] = self.results(inst).as_slice() else { return Ok(false) };
        let terms = Terms::new(self.func, inst, PLAIN, 32);
        let Some(found) = rules::TABLE.find(&terms, Term::Root) else { return Ok(false) };
        let rule = &rules::TABLE.rules[found.rule];
        let mut at = 0;
        self.build(rule.replacement, &mut at, &found.bindings)?;
        self.set(result);
        Ok(true)
    }

    /// Write the term of a replacement that starts at `pieces[*at]`: its arguments in order, and
    /// then the code of its head.
    fn build(&mut self, pieces: &[Piece], at: &mut usize, bindings: &[Term]) -> Result<()> {
        let piece = &pieces[*at];
        *at += 1;
        let (name, arity) = match *piece {
            Piece::Var { index, .. } => {
                let Term::Reg(value) = bindings[index] else {
                    return Err(format!(
                        "the rule binds {:?}, which is not a value",
                        bindings[index]
                    ));
                };
                return self.push(value);
            }
            Piece::App { head, arity } => (head, arity),
            Piece::Int(_) | Piece::Computed { .. } => {
                return Err(
                    "a wasm rule writes a constant, which the selector has no head for".into()
                );
            }
        };
        let head = rules::head(name).ok_or_else(|| format!("the rule head {name} has no code"))?;
        // A zero extension of a value that is already clean is no code, which is what `push_z`
        // does for the instructions that are not selected by rule. The mask of a shift count that
        // is a constant below the width keeps the whole constant, so it is no code either.
        if let Some(&Piece::Var { index, .. }) = pieces.get(*at) {
            if let Term::Reg(value) = bindings[index] {
                let bare = match head {
                    rules::Head::ZeroExtend(_) => self.clean(value),
                    rules::Head::Count(bits) => {
                        self.constant(value).is_some_and(|count| count < u128::from(bits))
                    }
                    _ => false,
                };
                if bare {
                    *at += 1;
                    return self.push(value);
                }
            }
        }
        for _ in 0..arity {
            self.build(pieces, at, bindings)?;
        }
        match head {
            rules::Head::Op(op) => self.code.op(op),
            rules::Head::SignExtend(bits) => self.sign_extend(bits),
            rules::Head::ZeroExtend(bits) => {
                self.code.i32_const(((1u32 << bits) - 1) as i32);
                self.code.op(emit::I32_AND);
            }
            rules::Head::Count(bits) => {
                self.code.i32_const(bits as i32 - 1);
                self.code.op(emit::I32_AND);
            }
            rules::Head::Low => {}
        }
        Ok(())
    }

    /// The shift count, in the width of the value it shifts. Only the low bits count, so a narrow
    /// count is cleared above its width first.
    fn shift_count(&mut self, count: Value, wide: bool) -> Result<()> {
        self.push_z(count)?;
        self.resize(self.wide(count), wide, false);
        Ok(())
    }

    fn float_base(&self, value: Value) -> Result<u8> {
        Ok(match valtype(self.ty(value))? {
            ValType::F32 => emit::F32_ADD,
            ValType::F64 => emit::F64_ADD,
            other => return Err(format!("float arithmetic on {other}")),
        })
    }

    /// A floating point comparison. wasm has the six ordered ones and `ne`, and the others are
    /// written from those.
    fn fcmp(&mut self, pred: FloatPred, a: Value, b: Value) -> Result<()> {
        let base: u8 = match valtype(self.ty(a))? {
            ValType::F32 => 0x5b,
            _ => 0x61,
        };
        let (eq, ne, lt, gt, le, ge) = (base, base + 1, base + 2, base + 3, base + 4, base + 5);
        let two = |s: &mut Self, op: u8| -> Result<()> {
            s.push(a)?;
            s.push(b)?;
            s.code.op(op);
            Ok(())
        };
        match pred {
            FloatPred::False => self.code.i32_const(0),
            FloatPred::True => self.code.i32_const(1),
            FloatPred::Oeq => two(self, eq)?,
            FloatPred::Ogt => two(self, gt)?,
            FloatPred::Oge => two(self, ge)?,
            FloatPred::Olt => two(self, lt)?,
            FloatPred::Ole => two(self, le)?,
            FloatPred::Une => two(self, ne)?,
            FloatPred::Ugt => {
                two(self, le)?;
                self.code.op(emit::I32_EQZ);
            }
            FloatPred::Uge => {
                two(self, lt)?;
                self.code.op(emit::I32_EQZ);
            }
            FloatPred::Ult => {
                two(self, ge)?;
                self.code.op(emit::I32_EQZ);
            }
            FloatPred::Ule => {
                two(self, gt)?;
                self.code.op(emit::I32_EQZ);
            }
            FloatPred::One | FloatPred::Ueq => {
                two(self, lt)?;
                two(self, gt)?;
                self.code.op(emit::I32_OR);
                if pred == FloatPred::Ueq {
                    self.code.op(emit::I32_EQZ);
                }
            }
            FloatPred::Ord | FloatPred::Uno => {
                let (same, join) =
                    if pred == FloatPred::Ord { (eq, emit::I32_AND) } else { (ne, emit::I32_OR) };
                for v in [a, b] {
                    self.push(v)?;
                    self.push(v)?;
                    self.code.op(same);
                }
                self.code.op(join);
            }
        }
        Ok(())
    }

    /// Read the cursor of a `va_list` into a new local and align it.
    fn va_cursor(&mut self, list: Value, align: u32) -> Result<u32> {
        let cursor = self.new_local(ValType::I32);
        self.push(list)?;
        self.code.mem(emit::I32_LOAD, 2, 0);
        if align > 4 {
            self.code.i32_const((align - 1) as i32);
            self.code.op(emit::I32_ADD);
            self.code.i32_const(-(align as i32));
            self.code.op(emit::I32_AND);
        }
        self.code.local_set(cursor);
        Ok(cursor)
    }

    /// Store the cursor of a `va_list` moved past `size` bytes.
    fn va_advance(&mut self, list: Value, cursor: u32, size: u32) {
        let _ = self.push(list);
        self.code.local_get(cursor);
        self.code.i32_const(size as i32);
        self.code.op(emit::I32_ADD);
        self.code.mem(emit::I32_STORE, 2, 0);
    }

    /// A call, direct or through an address, and a tail call when `tail` is set.
    fn call(&mut self, inst: Inst, tail: bool) -> Result<()> {
        let func = self.func;
        let Extra::Call(info) = func[inst].extra else { return Err("a call".into()) };
        let info = func[info];
        // wasi-libc has `setjmp` only in libsetjmp, which works through exception handling and
        // needs the compiler to write the matching code around each call. `prepare` does that,
        // and a call that is still here is one that it did not see.
        let callee = info.callee.map(|callee| self.unit.names.resolve(callee));
        let defined = |name| {
            matches!(self.unit.ir.lookup(name),
            Some(rucc_ir::SymbolRef::Func(f)) if !self.unit.ir[f].is_declaration())
        };
        if matches!(callee, Some("setjmp" | "_setjmp" | "sigsetjmp" | "__sigsetjmp"))
            && !info.callee.is_some_and(defined)
        {
            return Err("a call to `setjmp` that `rucc_wasm::prepare` did not change".into());
        }
        if let Some(name) = callee.filter(|&name| builtin::is_builtin(name)) {
            if !info.callee.is_some_and(defined) {
                let args = self.args(inst);
                self.builtin(name, &args)?;
                if tail {
                    self.epilogue();
                    self.code.op(emit::RETURN);
                } else {
                    match self.results(inst).first() {
                        Some(&result) => self.set(result),
                        None => self.code.op(emit::DROP),
                    }
                }
                return Ok(());
            }
        }
        let sig = &func[info.signature];
        let args = self.args(inst);
        let (address, args) = match info.callee {
            Some(_) => (None, &args[..]),
            None => (Some(args[0]), &args[1..]),
        };
        let params: Vec<_> = sig.params.iter().filter(|p| !p.ty.is_mem()).collect();
        if args.len() < params.len() {
            return Err("a call with fewer arguments than its signature".into());
        }
        // A pair comes back through the buffer of the frame, or straight to where this function
        // returns its own pair when the call is a tail call.
        let sret = sig.returns.iter().any(|ret| is_pair(ret.ty));
        match (sret, tail) {
            (true, true) if self.sret => self.code.local_get(0),
            (true, true) => return Err("a tail call that returns a pair to a caller".into()),
            (true, false) => self.scratch_address(0),
            (false, _) => {}
        }
        for (&value, param) in args.iter().zip(&params) {
            self.push_abi(value, param.abi)?;
        }
        if sig.variadic {
            let extra = &args[params.len()..];
            if extra.is_empty() {
                self.code.i32_const(0);
            } else {
                let (offsets, _) = va_layout(extra.iter().map(|&v| func[v].ty))?;
                let fp = self.frame_pointer();
                for (&value, offset) in extra.iter().zip(offsets) {
                    let ty = self.ty(value);
                    if is_pair(ty) {
                        self.store_pair_at(value, offset, 16, |s| {
                            s.code.local_get(fp);
                            Ok(())
                        })?;
                        continue;
                    }
                    let (op, natural) = store_op(ty)?;
                    self.code.local_get(fp);
                    if self.narrow(value).is_some() {
                        self.push_s(value)?;
                        self.code.mem(emit::I32_STORE, 2, offset);
                    } else {
                        self.push(value)?;
                        self.code.mem(op, align_field(natural, natural), offset);
                    }
                }
                self.code.local_get(fp);
            }
        } else if args.len() > params.len() {
            return Err("a call with more arguments than its signature".into());
        }
        let ty = functype(sig)?;
        let gives = !ty.results.is_empty();
        // A tail call is `return_call` when the target has the feature, and a call and a `return`
        // when it does not. `return_call` gives the frame back before the callee runs, so it is
        // not used for a variadic callee, whose arguments are in the buffer of this frame. It also
        // needs the callee to give back what this function gives back, type for type, and
        // `tail::mark` lets a function that gives back nothing drop the answer of its callee.
        let jump = tail
            && self.unit.features.has(Feature::TailCall)
            && !sig.variadic
            && ty.results == functype(func.signature())?.results;
        if jump {
            self.epilogue();
        }
        let ty = self.unit.out.intern(ty);
        match (info.callee, address) {
            (Some(callee), _) => {
                let (symbol, declared) = self.unit.function(callee).or_else(|_| {
                    let ty = self.unit.out.types[ty as usize].clone();
                    let name = self.unit.name(callee);
                    Ok::<_, String>(self.unit.libcall(&name, ty))
                })?;
                if declared == ty {
                    self.code.call(symbol, jump);
                } else {
                    // The call does not match the declaration, which C allows for a function
                    // declared with no prototype. A direct call of the wrong type would not
                    // validate, and a call through the table is checked when it runs.
                    self.code.address(RelocKind::TableIndexSleb, symbol, 0);
                    let table = self.unit.table();
                    self.code.call_indirect(ty, table, jump);
                }
            }
            (None, Some(address)) => {
                self.push(address)?;
                let table = self.unit.table();
                self.code.call_indirect(ty, table, jump);
            }
            (None, None) => return Err("a call with no callee".into()),
        }
        if jump {
            return Ok(());
        }
        if tail {
            // A `return` takes the values that the function gives back from the top of the stack
            // and leaves the rest, so an answer that `tail::mark` let this function drop needs no
            // `drop`.
            self.epilogue();
            self.code.op(emit::RETURN);
            return Ok(());
        }
        match self.results(inst).first() {
            Some(&result) if gives => self.set(result),
            Some(&result) if sret => self.load_scratch(result),
            _ if gives => self.code.op(emit::DROP),
            _ => {}
        }
        Ok(())
    }

    /// `memcpy`, `memmove` and `memset`. A short copy of a known size is loads and stores, and
    /// anything else is `memory.copy` and `memory.fill` when the target has them and a call to
    /// the C library when it does not.
    fn bulk(&mut self, inst: Inst, args: &[Value]) -> Result<()> {
        let opcode = self.func[inst].opcode;
        let mem = self.mem_info(inst);
        let align = mem.map_or(1, |m| m.align).max(1);
        let size = match args.get(2) {
            Some(&len) => self.constant(len).map(|n| n as u64),
            None => mem.map(|m| m.size),
        };
        if let Some(size @ ..=64) = size {
            if opcode == Opcode::Memset {
                if let Some(byte) = self.constant(args[1]) {
                    return self.fill_short(args[0], (byte & 0xff) as u8, size as u32, align);
                }
            } else if opcode == Opcode::Memcpy
                || size <= 8 && size.is_power_of_two() && u64::from(align) >= size
            {
                return self.copy_short(args[0], args[1], size as u32, align);
            }
        }
        self.push(args[0])?;
        if opcode == Opcode::Memset {
            self.push_z(args[1])?
        } else {
            self.push(args[1])?
        }
        match args.get(2) {
            Some(&len) => self.push_i32(len)?,
            None => {
                let size = size.unwrap_or(0);
                self.code
                    .i32_const(u32::try_from(size).map_err(|_| "a huge copy".to_owned())? as i32);
            }
        }
        if self.unit.features.has(Feature::BulkMemoryOpt) {
            self.code.bulk(if opcode == Opcode::Memset {
                emit::MEMORY_FILL
            } else {
                emit::MEMORY_COPY
            });
        } else {
            let name = match opcode {
                Opcode::Memcpy => "memcpy",
                Opcode::Memmove => "memmove",
                _ => "memset",
            };
            let ty = FuncType { params: vec![ValType::I32; 3], results: vec![ValType::I32] };
            let (symbol, _) = self.unit.libcall(name, ty);
            self.code.call(symbol, false);
            self.code.op(emit::DROP);
        }
        Ok(())
    }

    /// The widest piece of a short copy at offset `at` with `left` bytes to go.
    fn piece(left: u32, align: u32) -> u32 {
        let mut width = 8;
        while width > left || width > align {
            width /= 2;
        }
        width
    }

    fn copy_short(&mut self, to: Value, from: Value, size: u32, align: u32) -> Result<()> {
        // Every load comes before every store, so that the same code is right for a `memmove`
        // of a size small enough to be one piece.
        let mut at = 0;
        while at < size {
            let width = Self::piece(size - at, align);
            let (load, store, vt) = match width {
                8 => (emit::I64_LOAD, emit::I64_STORE, ValType::I64),
                4 => (emit::I32_LOAD, emit::I32_STORE, ValType::I32),
                2 => (emit::I32_LOAD16_U, emit::I32_STORE16, ValType::I32),
                _ => (emit::I32_LOAD8_U, emit::I32_STORE8, ValType::I32),
            };
            let _ = vt;
            let field = width.trailing_zeros();
            self.push(to)?;
            self.push(from)?;
            self.code.mem(load, field, at);
            self.code.mem(store, field, at);
            at += width;
        }
        Ok(())
    }

    fn fill_short(&mut self, to: Value, byte: u8, size: u32, align: u32) -> Result<()> {
        let mut at = 0;
        while at < size {
            let width = Self::piece(size - at, align);
            let field = width.trailing_zeros();
            self.push(to)?;
            if width == 8 {
                self.code.i64_const(i64::from_le_bytes([byte; 8]));
                self.code.mem(emit::I64_STORE, field, at);
            } else {
                self.code.i32_const(i32::from_le_bytes([byte; 4]));
                let store = match width {
                    4 => emit::I32_STORE,
                    2 => emit::I32_STORE16,
                    _ => emit::I32_STORE8,
                };
                self.code.mem(store, field, at);
            }
            at += width;
        }
        Ok(())
    }

    /// An atomic read, modify and write. A module without threads has one agent, so this is a
    /// load, the operation, and a store.
    fn rmw(&mut self, inst: Inst, args: &[Value], results: &[Value]) -> Result<()> {
        let Extra::Rmw(op, _) = self.func[inst].extra else { return Err("an rmw".into()) };
        let (ptr, val) = (args[0], args[1]);
        let ty = self.ty(val);
        let wide = self.wide(val);
        let (load, natural) = load_op(ty)?;
        let (store, _) = store_op(ty)?;
        let align = self.mem_info(inst).map_or(natural, |m| m.align);
        let field = align_field(align, natural);
        let old = match results.first() {
            Some(&old) => self.local[&old],
            None => self.new_local(valtype(ty)?),
        };
        self.push(ptr)?;
        self.code.mem(load, field, 0);
        self.code.local_set(old);
        self.push(ptr)?;
        let arith = |op: RmwOp| match op {
            RmwOp::Add => Some(emit::I32_ADD),
            RmwOp::Sub => Some(emit::I32_SUB),
            RmwOp::And | RmwOp::Nand => Some(emit::I32_AND),
            RmwOp::Or => Some(emit::I32_OR),
            RmwOp::Xor => Some(emit::I32_XOR),
            _ => None,
        };
        match op {
            RmwOp::Xchg => self.push(val)?,
            RmwOp::FAdd | RmwOp::FSub => {
                let base = self.float_base(val)?;
                self.code.local_get(old);
                self.push(val)?;
                self.code.op(base + u8::from(op == RmwOp::FSub));
            }
            RmwOp::SMax | RmwOp::SMin | RmwOp::UMax | RmwOp::UMin => {
                let signed = matches!(op, RmwOp::SMax | RmwOp::SMin);
                let pred = match op {
                    RmwOp::SMax => IntPred::Sgt,
                    RmwOp::SMin => IntPred::Slt,
                    RmwOp::UMax => IntPred::Ugt,
                    _ => IntPred::Ult,
                };
                let bits = self.narrow(val);
                let both = |s: &mut Self| -> Result<()> {
                    s.code.local_get(old);
                    if let (true, Some(bits)) = (signed, bits) {
                        s.sign_extend(bits);
                    }
                    if signed { s.push_s(val) } else { s.push_z(val) }
                };
                both(self)?;
                both(self)?;
                self.code.op(int_compare(pred, wide));
                self.code.op(emit::SELECT);
            }
            other => {
                let base = arith(other).ok_or_else(|| format!("atomic {}", other.name()))?;
                self.code.local_get(old);
                self.push(val)?;
                self.code.op(int_op(base, wide));
                if other == RmwOp::Nand {
                    if wide {
                        self.code.i64_const(-1)
                    } else {
                        self.code.i32_const(-1)
                    }
                    self.code.op(int_op(emit::I32_XOR, wide));
                }
            }
        }
        self.code.mem(store, field, 0);
        Ok(())
    }

    fn cmpxchg(&mut self, inst: Inst, args: &[Value], results: &[Value]) -> Result<()> {
        let (ptr, expected, new) = (args[0], args[1], args[2]);
        let ty = self.ty(new);
        let wide = self.wide(new);
        let (load, natural) = load_op(ty)?;
        let (store, _) = store_op(ty)?;
        let align = self.mem_info(inst).map_or(natural, |m| m.align);
        let field = align_field(align, natural);
        let old = match results.first() {
            Some(&old) => self.local[&old],
            None => self.new_local(valtype(ty)?),
        };
        let ok = match results.get(1) {
            Some(&ok) => self.local[&ok],
            None => self.new_local(ValType::I32),
        };
        self.push(ptr)?;
        self.code.mem(load, field, 0);
        self.code.local_tee(old);
        self.push_z(expected)?;
        self.code.op(int_compare(IntPred::Eq, wide));
        self.code.local_tee(ok);
        self.code.open(emit::IF, None);
        self.push(ptr)?;
        self.push(new)?;
        self.code.mem(store, field, 0);
        self.code.op(emit::END);
        Ok(())
    }

    /// An operation with a flag that says whether it overflowed.
    fn overflow(&mut self, opcode: Opcode, args: &[Value], inst: Inst) -> Result<()> {
        let all: Vec<Value> = self.func[inst].results().collect();
        let (result, flag) = (all[0], all[1]);
        let (a, b) = (args[0], args[1]);
        let wide = self.wide(a);
        let r = self.local[&result];
        let o = self.local[&flag];
        let signed =
            matches!(opcode, Opcode::SAddOverflow | Opcode::SSubOverflow | Opcode::SMulOverflow);
        let op = match opcode {
            Opcode::SAddOverflow | Opcode::UAddOverflow => emit::I32_ADD,
            Opcode::SSubOverflow | Opcode::USubOverflow => emit::I32_SUB,
            _ => emit::I32_MUL,
        };

        // A narrow operation is done in 32 bits on extended operands, and it overflowed when the
        // answer does not fit back in the narrow type.
        if let Some(bits) = self.narrow(a) {
            if signed {
                self.push_s(a)?
            } else {
                self.push_z(a)?
            }
            if signed {
                self.push_s(b)?
            } else {
                self.push_z(b)?
            }
            self.code.op(op);
            self.code.local_set(r);
            self.code.local_get(r);
            if signed {
                self.sign_extend(bits);
                self.code.local_get(r);
                self.code.op(emit::I32_NE);
            } else {
                self.code.i32_const(((1u32 << bits) - 1) as i32);
                self.code.op(emit::I32_GT_U);
            }
            self.code.local_set(o);
            return Ok(());
        }

        if op == emit::I32_MUL && !wide {
            // In 64 bits, where the product of two 32-bit numbers always fits.
            let p = self.new_local(ValType::I64);
            for v in [a, b] {
                self.push(v)?;
                self.resize(false, true, signed);
            }
            self.code.op(int_op(emit::I32_MUL, true));
            self.code.local_tee(p);
            self.code.op(emit::I32_WRAP_I64);
            self.code.local_set(r);
            self.code.local_get(p);
            if signed {
                self.code.local_get(r);
                self.code.op(emit::I64_EXTEND_I32_S);
                self.code.op(emit::I64_NE);
            } else {
                self.code.i64_const(32);
                self.code.op(int_op(emit::I32_SHR_U, true));
                self.code.i64_const(0);
                self.code.op(emit::I64_NE);
            }
            self.code.local_set(o);
            return Ok(());
        }

        self.push(a)?;
        self.push(b)?;
        self.code.op(int_op(op, wide));
        self.code.local_set(r);
        let zero = |s: &mut Self| if wide { s.code.i64_const(0) } else { s.code.i32_const(0) };
        match opcode {
            Opcode::SAddOverflow | Opcode::SSubOverflow => {
                // For a sum, it overflowed when the answer differs in sign from both operands,
                // which is (a ^ r) & (b ^ r) < 0. For a difference, it overflowed when the
                // operands differ in sign and the answer differs from the first, which is
                // (a ^ r) & (a ^ b) < 0.
                self.push(a)?;
                self.code.local_get(r);
                self.code.op(int_op(emit::I32_XOR, wide));
                if opcode == Opcode::SAddOverflow {
                    self.push(b)?;
                    self.code.local_get(r);
                } else {
                    self.push(a)?;
                    self.push(b)?;
                }
                self.code.op(int_op(emit::I32_XOR, wide));
                self.code.op(int_op(emit::I32_AND, wide));
                zero(self);
                self.code.op(if wide { emit::I64_LT_S } else { emit::I32_LT_S });
            }
            Opcode::UAddOverflow => {
                self.code.local_get(r);
                self.push(a)?;
                self.code.op(if wide { emit::I64_LT_U } else { emit::I32_LT_U });
            }
            Opcode::USubOverflow => {
                self.push(a)?;
                self.push(b)?;
                self.code.op(if wide { emit::I64_LT_U } else { emit::I32_LT_U });
            }
            Opcode::UMulOverflow => {
                // a != 0 && r / a != b, with no division when a is zero.
                self.push(a)?;
                self.code.op(emit::I64_EQZ);
                self.code.open(emit::IF, Some(ValType::I32));
                self.code.i32_const(0);
                self.code.op(emit::ELSE);
                self.code.local_get(r);
                self.push(a)?;
                self.code.op(int_op(emit::I32_DIV_U, true));
                self.push(b)?;
                self.code.op(emit::I64_NE);
                self.code.op(emit::END);
            }
            _ => {
                // Signed: no overflow when a is zero, overflow when a is -1 and b is the least
                // value, and otherwise r / a != b, where the division cannot trap.
                self.push(a)?;
                self.code.op(emit::I64_EQZ);
                self.code.open(emit::IF, Some(ValType::I32));
                self.code.i32_const(0);
                self.code.op(emit::ELSE);
                self.push(a)?;
                self.code.i64_const(-1);
                self.code.op(emit::I64_EQ);
                self.code.open(emit::IF, Some(ValType::I32));
                self.push(b)?;
                self.code.i64_const(i64::MIN);
                self.code.op(emit::I64_EQ);
                self.code.op(emit::ELSE);
                self.code.local_get(r);
                self.push(a)?;
                self.code.op(int_op(emit::I32_DIV_S, true));
                self.push(b)?;
                self.code.op(emit::I64_NE);
                self.code.op(emit::END);
                self.code.op(emit::END);
            }
        }
        self.code.local_set(o);
        Ok(())
    }

    /// The bytes of an integer in the other order.
    fn bswap(&mut self, value: Value, result: Value) -> Result<()> {
        match self.ty(value).bits() {
            16 => {
                self.push_z(value)?;
                self.code.i32_const(8);
                self.code.op(emit::I32_SHL);
                self.push_z(value)?;
                self.code.i32_const(8);
                self.code.op(emit::I32_SHR_U);
                self.code.op(emit::I32_OR);
            }
            32 => {
                // rotr(x & 0xff00ff00, 8) | rotl(x & 0x00ff00ff, 8)
                self.push(value)?;
                self.code.i32_const(0xff00_ff00_u32 as i32);
                self.code.op(emit::I32_AND);
                self.code.i32_const(8);
                self.code.op(emit::I32_ROTR);
                self.push(value)?;
                self.code.i32_const(0x00ff_00ff);
                self.code.op(emit::I32_AND);
                self.code.i32_const(8);
                self.code.op(emit::I32_ROTL);
                self.code.op(emit::I32_OR);
            }
            64 => {
                let t = self.new_local(ValType::I64);
                self.push(value)?;
                self.code.local_set(t);
                for (shift, mask) in [(8, 0x00ff_00ff_00ff_00ff_i64), (16, 0x0000_ffff_0000_ffff)] {
                    self.code.local_get(t);
                    self.code.i64_const(mask);
                    self.code.op(int_op(emit::I32_AND, true));
                    self.code.i64_const(shift);
                    self.code.op(int_op(emit::I32_SHL, true));
                    self.code.local_get(t);
                    self.code.i64_const(shift);
                    self.code.op(int_op(emit::I32_SHR_U, true));
                    self.code.i64_const(mask);
                    self.code.op(int_op(emit::I32_AND, true));
                    self.code.op(int_op(emit::I32_OR, true));
                    self.code.local_set(t);
                }
                self.code.local_get(t);
                self.code.i64_const(32);
                self.code.op(int_op(emit::I32_ROTL, true));
            }
            bits => return Err(format!("a byte swap of {bits} bits")),
        }
        self.set(result);
        Ok(())
    }
}
