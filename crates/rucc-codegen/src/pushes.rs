//! The stack arguments of an i386 call pushed rather than stored, where that is fewer bytes.
//!
//! The lowering stores each argument a call passes on the stack into the outgoing area at the
//! bottom of the frame, `movl %eax, 4(%esp)` and `movl $.LC0, (%esp)`, which is what gcc writes at
//! -O2. At -Os gcc pushes them instead, last first, and takes them off again after the call:
//!
//! ```text
//!   movl $.LC0, (%esp)      7 bytes          pushl %eax      1 byte
//!   movl %eax, 4(%esp)      4 bytes          pushl $.LC0     5 bytes
//!   call printf                              call printf
//!                                            addl $8, %esp   3 bytes
//! ```
//!
//! A push of a register is one byte where the store is three or four, and a push of a constant is
//! two or five where the store is seven or eight, so all but a call of one register argument comes
//! out shorter, and a callee that takes its own arguments off the stack, which is every
//! non-variadic call under `-mrtd` and every `stdcall` one, also loses the `subl` the lowering
//! writes behind it to put the outgoing area back. FreeBSD's boot2 passes every `printf` argument
//! this way and has to fit in 7.5 KiB. tamnd/rucc-bsd#7.
//!
//! It runs after the allocator, the frame and every pass that moves an instruction, since what it
//! asks is about registers and about where the stack pointer is: each store is one of the call's,
//! nothing between the stores and the call touches the outgoing area, and together the stores write
//! the whole of the area the call passes, which [`crate::lower`] wrote down for each call as it
//! built it. The pushes go straight in front of the call when nothing between a store and the call
//! writes the register it stores. When something does, the push of that register goes in front of
//! the instruction that writes it, as gcc does, and each address counted from the stack pointer
//! between the first push and the call is moved by what was pushed in front of it. The unwind table
//! gets a row for each push and one for the way back when it counts the frame from the stack
//! pointer.

use rucc_base::Interner;
use rucc_mir as mir;
use rucc_target::{CallRegs, FrameInsts, MachineInsts};

/// The opcode of a push of a constant, which only i386's frame writes.
const PUSH_IMM: &str = "push_i_32";

/// Rewrites the stack arguments of each of those calls as pushes where that is fewer bytes, and
/// gives back what it rewrote.
///
/// `calls` is each call that passes arguments on the stack with how many bytes of the outgoing area
/// they take, which is `passing` on [`crate::lower::Stack`]. A call is left alone when its area is
/// not a number of words. When the area is not a multiple of the stack alignment, a pad of the
/// difference goes in front of the pushes and the `addl` behind the call takes off the area and the
/// difference, as gcc does at -Os. A call whose callee takes its own area off is then left alone.
pub fn arguments(
    func: &mut mir::Func,
    calls: &[(mir::Inst, u32)],
    insts: &FrameInsts,
    machine: &MachineInsts,
    conv: &CallRegs,
    names: &mut Interner,
) -> Pushed {
    let ops = Ops::of(insts, names);
    let sp = mir::Reg::physical(conv.stack_pointer);
    let mut pushed = Pushed { calls: 0, back: Vec::new() };
    for &(call, area) in calls {
        if area == 0 || area % 4 != 0 {
            continue;
        }
        let pad = (conv.stack_align - area % conv.stack_align) % conv.stack_align;
        let Some((plan, change)) = plan(func, call, area, pad, sp, &ops, machine, names) else {
            continue;
        };
        if change >= 0 {
            continue;
        }
        let at = cfa(func, call, conv);
        pushed.back.extend(rewrite(func, call, area, pad, &plan, &ops, conv, at));
        pushed.calls += 1;
    }
    pushed
}

/// Takes the outgoing area out of a frame that holds nothing else, once [`arguments`] has pushed
/// what each call passed in it, and says whether it did.
///
/// `size` is how many bytes the prologue takes off the stack pointer and `outgoing` is how many of
/// those are the area. When the size is the area rounded up to the call alignment and no
/// instruction reads the frame any longer, the `subl $8, %esp` of the prologue and the
/// `addl $8, %esp` of each epilogue are six bytes that do nothing, and gcc has neither at -Os. Each
/// address counted from the stack pointer is then that many bytes nearer to it.
///
/// The bytes above the area are the pad that keeps the stack pointer on the call alignment, and
/// nothing is in them. The `readsects` call in NetBSD's `bootxx` passes 24 bytes and its frame is
/// 32. Without the frame, the pushes are padded to the call alignment as each call is, so the
/// stack pointer is on it at the call as before.
///
/// The function is left alone when the stack pointer is written by anything but the prologue, an
/// epilogue, a push, a pop, a call and the `back` of [`Pushed`], or read other than as the base of
/// an address or by a call, since each of those counts the frame from where the prologue left the
/// stack pointer. Each row of the unwind table inside the frame is moved by the same bytes, and the
/// function is left alone when a row counts the frame from another register.
#[allow(clippy::too_many_arguments)]
pub fn unframed(
    func: &mut mir::Func,
    size: u32,
    outgoing: u32,
    pushed: &Pushed,
    insts: &FrameInsts,
    machine: &MachineInsts,
    conv: &CallRegs,
    names: &mut Interner,
) -> bool {
    size == outgoing.next_multiple_of(conv.stack_align)
        && unframe(func, size, pushed, insts, machine, conv, names).is_some()
}

/// Takes out the frame of `size` bytes as [`unframed`] does, when nothing reads it, and gives back
/// how many instructions that took out: the `subl` of the prologue and the `addl` of each epilogue.
fn unframe(
    func: &mut mir::Func,
    size: u32,
    pushed: &Pushed,
    insts: &FrameInsts,
    machine: &MachineInsts,
    conv: &CallRegs,
    names: &mut Interner,
) -> Option<usize> {
    if size == 0 {
        return None;
    }
    let Ok(bytes) = i32::try_from(size) else { return None };
    let sub = mir::Opcode::new(names.join(insts.prefix, insts.sub));
    let add = mir::Opcode::new(names.join(insts.prefix, insts.add));
    // What moves the stack pointer by a word or takes the return address off it, which leaves
    // each address counted from it the same distance from the frame either way.
    let mut moving = vec![
        mir::Opcode::new(names.join(insts.prefix, insts.push)),
        mir::Opcode::new(names.join(insts.prefix, PUSH_IMM)),
        mir::Opcode::new(names.join(insts.prefix, insts.pop)),
        mir::Opcode::new(names.join(insts.prefix, insts.ret)),
    ];
    if let Some(ret_pop) = insts.ret_pop {
        moving.push(mir::Opcode::new(names.join(insts.prefix, ret_pop)));
    }
    let sp = mir::Reg::physical(conv.stack_pointer);
    let entry = func.entry()?;
    let mut prologue = None;
    let mut epilogues = Vec::new();
    let mut based = Vec::new();
    for block in func.blocks() {
        for inst in func.insts(block) {
            let data = &func[inst];
            let operands = &func[data.operands];
            let base = match data.mem {
                Some(mem) => {
                    let amode = func[mem];
                    let names_sp = |at: Option<u8>| {
                        at.and_then(|at| operands.get(usize::from(at)))
                            .is_some_and(|op| op.reg == sp)
                    };
                    if names_sp(amode.index) {
                        return None;
                    }
                    if names_sp(amode.base) {
                        // A word of the area still read or written.
                        if amode.disp < bytes {
                            return None;
                        }
                        if !based.contains(&mem) {
                            based.push(mem);
                        }
                    }
                    amode.base
                }
                None => None,
            };
            let writes = operands.iter().any(|op| op.reg == sp && op.role != mir::Role::Use);
            let reads = operands.iter().enumerate().any(|(at, op)| {
                op.reg == sp && op.role == mir::Role::Use && base != u8::try_from(at).ok()
            });
            let imm = data.imm.map(|imm| func[imm].0);
            if moving.contains(&data.opcode) || machine.calls(names.resolve(data.opcode.name())) {
                continue;
            }
            if writes {
                if pushed.back.contains(&inst) {
                    continue;
                }
                if data.opcode == sub
                    && imm == Some(i64::from(size))
                    && block == entry
                    && prologue.is_none()
                {
                    prologue = Some(inst);
                } else if data.opcode == add && imm == Some(i64::from(size)) {
                    epilogues.push(inst);
                } else {
                    return None;
                }
            } else if reads {
                return None;
            }
        }
    }
    let prologue = prologue?;
    // Each row of the unwind table counts the frame from the stack pointer here, so a row inside
    // the frame is that many bytes nearer to it and the rows of the two instructions go with them.
    if !func.cfi.is_empty() {
        let frame = func.cfi.iter().find_map(|&(inst, op)| match op {
            mir::CfiOp::DefCfaOffset(offset) if inst == prologue => Some(offset),
            _ => None,
        });
        let counted = func.cfi.iter().all(|(_, op)| {
            !matches!(op, mir::CfiOp::DefCfa { .. } | mir::CfiOp::DefCfaRegister(_))
        });
        let frame = frame.filter(|_| counted)?;
        let remembers = func.cfi_after(prologue).any(|op| op == mir::CfiOp::RememberState);
        func.cfi.retain(|(inst, _)| *inst != prologue && !epilogues.contains(inst));
        // The body's rules, which each epilogue that is not the last one puts back after its
        // `ret`, were remembered on the `subl`. They are now the rules the instruction in front
        // of it leaves, so they are remembered there. With nothing in front, the body runs under
        // the rules every function starts with, which a `ret` leaves too, and nothing is kept.
        if remembers {
            match func.insts(entry).take_while(|&inst| inst != prologue).last() {
                Some(before) => func.cfi.push((before, mir::CfiOp::RememberState)),
                None => func.cfi.retain(|(_, op)| {
                    !matches!(op, mir::CfiOp::RememberState | mir::CfiOp::RestoreState)
                }),
            }
        }
        for (_, op) in &mut func.cfi {
            if let mir::CfiOp::DefCfaOffset(offset) = op {
                if *offset >= frame {
                    *offset -= bytes;
                }
            }
        }
    }
    let gone = 1 + epilogues.len();
    func.remove_inst(prologue);
    for epilogue in epilogues {
        func.remove_inst(epilogue);
    }
    for mem in based {
        func[mem].disp -= bytes;
    }
    Some(gone)
}

/// Pushes the arguments of each call and takes the frame out, when the frame is the outgoing area
/// and a pad smaller than the call alignment, and says whether it did.
///
/// [`unframed`] takes out a frame that is the area rounded up to the call alignment. A frame can
/// also be smaller than that, because the return address and the saved registers are on the stack
/// too. `ob` in NetBSD's `bootxx` saves no register, and its frame is 12 bytes for an 8-byte area,
/// so the 4 bytes of the return address and the 12 of the frame align the stack pointer at the
/// call. gcc writes no frame there and pushes the two arguments under a pad of 4 bytes.
///
/// Without the frame, the pad of each call is what puts the stack pointer where the frame put it:
/// the frame less the area of the call, less whole steps of the alignment. A call that passes
/// nothing on the stack would find the stack pointer off the alignment, so each call has to be one
/// of `calls`, and each has to be pushed. That costs bytes where [`arguments`] keeps the stores, so
/// the bytes of the pushes, less the `subl` of the prologue and the `addl` of each epilogue, are
/// set against what [`arguments`] saves with the frame kept. The function is left alone unless
/// this is fewer bytes, and when [`unframed`] would find the frame still read.
#[allow(clippy::too_many_arguments)]
pub fn frameless(
    func: &mut mir::Func,
    size: u32,
    outgoing: u32,
    calls: &[(mir::Inst, u32)],
    insts: &FrameInsts,
    machine: &MachineInsts,
    conv: &CallRegs,
    names: &mut Interner,
) -> bool {
    let align = conv.stack_align;
    if outgoing == 0 || size < outgoing || size - outgoing >= align {
        return false;
    }
    let listed = |inst: mir::Inst| calls.iter().any(|&(call, area)| call == inst && area > 0);
    let unlisted = func.blocks().any(|block| {
        func.insts(block)
            .any(|inst| machine.calls(names.resolve(func[inst].opcode.name())) && !listed(inst))
    });
    if unlisted {
        return false;
    }
    let ops = Ops::of(insts, names);
    let sp = mir::Reg::physical(conv.stack_pointer);
    let mut trial = func.clone();
    let mut pushed = Pushed { calls: 0, back: Vec::new() };
    // The bytes the pushes add here, and the bytes the pushes that `arguments` chooses take away.
    let mut change = 0;
    let mut kept = 0;
    for &(call, area) in calls {
        if area == 0 {
            continue;
        }
        if area % 4 != 0 {
            return false;
        }
        let pad = (size - area) % align;
        let Some((planned, added)) = plan(&trial, call, area, pad, sp, &ops, machine, names) else {
            return false;
        };
        let aligned = (align - area % align) % align;
        if let Some((_, framed)) = plan(func, call, area, aligned, sp, &ops, machine, names) {
            kept += framed.min(0);
        }
        change += added;
        let at = cfa(&trial, call, conv);
        pushed.back.extend(rewrite(&mut trial, call, area, pad, &planned, &ops, conv, at));
        pushed.calls += 1;
    }
    if pushed.calls == 0 {
        return false;
    }
    let Some(gone) = unframe(&mut trial, size, &pushed, insts, machine, conv, names) else {
        return false;
    };
    let Ok(saved) = isize::try_from(gone * arith_bytes(size)) else { return false };
    if change - saved >= kept {
        return false;
    }
    *func = trial;
    true
}

/// What [`arguments`] rewrote.
#[derive(Debug)]
pub struct Pushed {
    /// How many calls.
    pub calls: usize,
    /// The `addl` behind each of those calls that takes its arguments off again, which a callee
    /// that takes them off itself has none of, and the `subl` in front of the pushes that keeps the
    /// stack pointer aligned at the call.
    pub back: Vec<mir::Inst>,
}

/// The opcodes the rewrite reads and writes.
struct Ops {
    store: mir::Opcode,
    store_imm: mir::Opcode,
    push: mir::Opcode,
    push_imm: mir::Opcode,
    add: mir::Opcode,
    sub: mir::Opcode,
}

impl Ops {
    /// The opcodes of that frame.
    fn of(insts: &FrameInsts, names: &mut Interner) -> Self {
        let mut opcode = |name: &'static str| mir::Opcode::new(names.join(insts.prefix, name));
        Self {
            store: opcode("mov_mr_32"),
            store_imm: opcode("mov_mi_32"),
            push: opcode(insts.push),
            push_imm: opcode(PUSH_IMM),
            add: opcode(insts.add),
            sub: opcode(insts.sub),
        }
    }
}

/// What one call's arguments become.
struct Plan {
    /// The store of each word of the area, from the bottom of it up.
    stores: Vec<mir::Inst>,
    /// The `subl` behind the call that puts back what a callee that cleans up took, which goes.
    popped: Option<mir::Inst>,
    /// The instruction the push of each word goes in front of, from the bottom of the area up.
    anchors: Vec<mir::Inst>,
    /// Each address counted from the stack pointer that a push moves, with its new displacement.
    moved: Vec<(mir::MemRef, i32)>,
    /// What goes below the area.
    fill: Pad,
}

/// The stores of that call's arguments, when they can be pushes, and how many bytes the pushes
/// add, which is less than zero when they are fewer bytes than the stores.
///
/// `pad` is how many bytes go below the area so the stack pointer is aligned at the call.
#[allow(clippy::too_many_arguments)]
fn plan(
    func: &mir::Func,
    call: mir::Inst,
    area: u32,
    pad: u32,
    sp: mir::Reg,
    ops: &Ops,
    machine: &MachineInsts,
    names: &Interner,
) -> Option<(Plan, isize)> {
    let block = func.block_of(call)?;
    if !machine.calls(names.resolve(func[call].opcode.name())) {
        return None;
    }
    // A call through an address counted from the stack pointer would read it from four bytes
    // further down for every push. A call also names the stack pointer as an operand it reads
    // and writes, and that is no obstacle: the pushes are in front of it.
    let operands = &func[func[call].operands];
    if let Some(mem) = func[call].mem {
        let amode = func[mem];
        let reads_sp = |at: Option<u8>| {
            at.and_then(|at| operands.get(usize::from(at))).is_some_and(|op| op.reg == sp)
        };
        if reads_sp(amode.base) || reads_sp(amode.index) {
            return None;
        }
    }
    let words = usize::try_from(area / 4).ok()?;
    let mut stores: Vec<Option<mir::Inst>> = vec![None; words];
    let mut found = 0;
    let order: Vec<mir::Inst> = func.insts(block).collect();
    let at = order.iter().position(|&inst| inst == call)?;
    for &inst in order[..at].iter().rev() {
        if found == words {
            break;
        }
        let data = &func[inst];
        let operands = &func[data.operands];
        if let Some(word) = word_stored(func, inst, area, sp, ops) {
            let source = (data.opcode == ops.store).then(|| operands[0].reg);
            if stores[word].is_some() || source == Some(sp) || func.cfi_after(inst).next().is_some()
            {
                return None;
            }
            stores[word] = Some(inst);
            found += 1;
            continue;
        }
        // The arrival of an argument in a register encodes to nothing and only writes that
        // register, which the walk notes like any other write.
        let name = names.resolve(data.opcode.name());
        let described = machine.has(name) || machine.arrives(name);
        if machine.calls(name) || !described || func.cfi_after(inst).next().is_some() {
            return None;
        }
        // The stack pointer as anything but the base of an address above the area: written, read
        // as a number, or the base of an address in the area, which is the address of a word this
        // would move or a read of one before it is written.
        let amode = data.mem.map(|mem| func[mem]);
        for (index, operand) in operands.iter().enumerate() {
            if operand.reg != sp {
                continue;
            }
            let based = amode.is_some_and(|amode| {
                amode.base.map(usize::from) == Some(index)
                    && amode.index.is_none()
                    && i64::from(amode.disp) >= i64::from(area)
            });
            if operand.role.is_def() || !based {
                return None;
            }
        }
    }
    let stores: Vec<mir::Inst> = stores.into_iter().collect::<Option<_>>()?;
    let places: Vec<usize> = stores
        .iter()
        .map(|&store| order.iter().position(|&inst| inst == store))
        .collect::<Option<_>>()?;
    let start = places.iter().copied().min()?;
    let (anchors, moved, grown) =
        places_of(func, &order[start..=at], &stores, &places, start, pad, sp, ops)?;
    // Behind the call, the `subl` that gives a callee that cleans up its area back, or nothing.
    let popped = func.next_inst(call).filter(|&next| {
        func[next].opcode == ops.sub
            && func[func[next].operands].iter().any(|op| op.role.is_def() && op.reg == sp)
    });
    if popped.is_some_and(|sub| {
        pad != 0
            || func[sub].imm.map(|imm| func[imm].0) != Some(i64::from(area))
            || func.cfi_after(sub).next().is_some()
    }) {
        return None;
    }
    // A pad pushed in front of a push that is not at the call can only be of the register of the
    // top word, which is the one register sure to be set there.
    let placed = anchors.iter().any(|&anchor| anchor != call);
    let fill = padding(func, if placed { &stores[words - 1..] } else { &stores }, pad, ops);
    let before: usize = stores.iter().map(|&store| stored_bytes(func, store, ops)).sum();
    let after: usize = stores.iter().map(|&store| pushed_bytes(func, store, ops)).sum::<usize>()
        + fill.bytes()
        + grown
        + if popped.is_some() { 0 } else { arith_bytes(area + pad) };
    let gone = if popped.is_some() { arith_bytes(area) } else { 0 };
    let change = isize::try_from(after).ok()? - isize::try_from(before + gone).ok()?;
    Some((Plan { stores, popped, anchors, moved, fill }, change))
}

/// Where the push of each store goes, the new displacement of each address counted from the stack
/// pointer that the pushes move, and how many bytes longer those addresses get.
///
/// `run` is the instructions from the first store to the call, which is the last of them, and
/// `places` is where each store is in the block, from the bottom word of the area up. The push of
/// a word goes in front of the call, or in front of the first instruction behind its store that
/// writes the register it pushes, and never behind the push of a word under it, since the top word
/// is pushed first. It goes after the last instruction in front of its store that writes that
/// register, and the call is left alone when that cannot be.
///
/// An address counted from the stack pointer behind a push is the number of bytes pushed in front
/// of it further from the stack pointer, the pad too. The call is left alone when an address that
/// moves is also the address of another instruction, which would move with it.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn places_of(
    func: &mir::Func,
    run: &[mir::Inst],
    stores: &[mir::Inst],
    places: &[usize],
    start: usize,
    pad: u32,
    sp: mir::Reg,
    ops: &Ops,
) -> Option<(Vec<mir::Inst>, Vec<(mir::MemRef, i32)>, usize)> {
    let call = run.len() - 1;
    let writes = |at: usize, reg: mir::Reg| {
        func[func[run[at]].operands].iter().any(|op| op.role.is_def() && op.reg == reg)
    };
    let mut at = Vec::with_capacity(stores.len());
    let mut latest = call;
    for (&store, &place) in stores.iter().zip(places) {
        let place = place - start;
        let data = &func[store];
        let (first, last) = if data.opcode == ops.store {
            let reg = func[data.operands][0].reg;
            (
                (0..place).rev().find(|&i| writes(i, reg)).map_or(0, |i| i + 1),
                (place + 1..call).find(|&i| writes(i, reg)).unwrap_or(call),
            )
        } else {
            (0, call)
        };
        latest = latest.min(last);
        if latest < first {
            return None;
        }
        at.push(latest);
    }
    let top = *at.last()?;
    let mut moved: Vec<(mir::MemRef, i32)> = Vec::new();
    let mut grown = 0;
    for (index, &inst) in run.iter().enumerate().take(call).skip(top) {
        let data = &func[inst];
        let Some(mem) = data.mem else { continue };
        let amode = func[mem];
        let based = amode
            .base
            .and_then(|base| func[data.operands].get(usize::from(base)))
            .is_some_and(|op| op.reg == sp);
        if stores.contains(&inst) || !based {
            continue;
        }
        let pushes = at.iter().filter(|&&anchor| anchor <= index).count();
        let shift = i32::try_from(pad).ok()? + 4 * i32::try_from(pushes).ok()?;
        let disp = amode.disp.checked_add(shift)?;
        if moved.iter().any(|&(other, _)| other == mem) {
            return None;
        }
        grown += disp_bytes(disp).saturating_sub(disp_bytes(amode.disp));
        moved.push((mem, disp));
    }
    if !moved.is_empty() {
        let shifted = &run[top..call];
        let shared = func.blocks().flat_map(|block| func.insts(block)).any(|inst| {
            !shifted.contains(&inst)
                && func[inst].mem.is_some_and(|mem| moved.iter().any(|&(other, _)| other == mem))
        });
        if shared {
            return None;
        }
    }
    Some((at.iter().map(|&index| run[index]).collect(), moved, grown))
}

/// Which word of the area that instruction stores, when it is a four byte store of a register or a
/// constant into it.
fn word_stored(
    func: &mir::Func,
    inst: mir::Inst,
    area: u32,
    sp: mir::Reg,
    ops: &Ops,
) -> Option<usize> {
    let data = &func[inst];
    if data.opcode != ops.store && data.opcode != ops.store_imm {
        return None;
    }
    let amode = func[data.mem?];
    let base = func[data.operands].get(usize::from(amode.base?))?;
    let plain = amode.index.is_none()
        && amode.symbol.is_none()
        && amode.block.is_none()
        && amode.table.is_none()
        && amode.segment.is_none();
    let disp = u32::try_from(amode.disp).ok()?;
    (base.reg == sp && plain && disp < area && disp % 4 == 0)
        .then(|| usize::try_from(disp / 4).ok())
        .flatten()
}

/// How many bytes the displacement of an address counted from the stack pointer takes.
fn disp_bytes(disp: i32) -> usize {
    match disp {
        0 => 0,
        _ if i8::try_from(disp).is_ok() => 1,
        _ => 4,
    }
}

/// How many bytes a store into the area is: the opcode, the `ModRM` and `SIB` bytes the stack
/// pointer needs, the displacement and the constant.
fn stored_bytes(func: &mir::Func, store: mir::Inst, ops: &Ops) -> usize {
    let data = &func[store];
    let disp = data.mem.map_or(0, |mem| func[mem].disp);
    let constant = if data.opcode == ops.store_imm { 4 } else { 0 };
    3 + disp_bytes(disp) + constant
}

/// How many bytes the push of what that store writes is.
fn pushed_bytes(func: &mir::Func, store: mir::Inst, ops: &Ops) -> usize {
    let data = &func[store];
    if data.opcode == ops.store {
        return 1;
    }
    let small =
        data.symbol.is_none() && data.imm.is_some_and(|imm| i8::try_from(func[imm].0).is_ok());
    if small { 2 } else { 5 }
}

/// How many bytes an `addl` or `subl` of that many bytes to the stack pointer is.
fn arith_bytes(area: u32) -> usize {
    if area <= 127 { 3 } else { 6 }
}

/// What goes below the area of a call so that the stack pointer is aligned at the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pad {
    /// Nothing, because the area is aligned.
    None,
    /// That many pushes of a register, or of the constant 0 when there is no register.
    Pushes(Option<mir::Operand>, u32),
    /// A `subl` of that many bytes.
    Sub(u32),
}

impl Pad {
    /// How many bytes the pad is.
    fn bytes(self) -> usize {
        match self {
            Self::None => 0,
            Self::Pushes(Some(_), words) => words as usize,
            Self::Pushes(None, words) => 2 * words as usize,
            Self::Sub(pad) => arith_bytes(pad),
        }
    }
}

/// The pad of `pad` bytes below the area those stores write.
///
/// One or two words are pushes of a register that an argument is pushed from, as the `-Os`
/// peepholes of gcc write: a `pushl %reg` is one byte where the `subl` is three. The register
/// holds what the call passes, because nothing between the store and the call writes it, so the
/// push reads a register that is set. One word and no register is a `pushl $0` of two bytes. A
/// larger pad is a `subl`.
fn padding(func: &mir::Func, stores: &[mir::Inst], pad: u32, ops: &Ops) -> Pad {
    let register = stores
        .iter()
        .rev()
        .find(|&&store| func[store].opcode == ops.store)
        .map(|&store| func[func[store].operands][0]);
    match (pad, register) {
        (0, _) => Pad::None,
        (4, _) | (8, Some(_)) => Pad::Pushes(register, pad / 4),
        _ => Pad::Sub(pad),
    }
}

/// How far above the stack pointer the unwind table says the frame is at the call, or `None` when
/// it counts the frame from another register, which no push changes.
///
/// The rows are read in the order the instructions are laid out, which is the order the unwinder
/// reads them in, from what every function starts with: the frame is the return address above the
/// stack pointer.
///
/// The state the prologue remembers is kept too, because the block after a `ret` starts from it
/// and not from where the epilogue left the frame.
fn cfa(func: &mir::Func, call: mir::Inst, conv: &CallRegs) -> Option<i32> {
    let sp = conv.dwarf(conv.int_class, conv.stack_pointer)?;
    let mut reg = sp;
    let mut offset = i32::try_from(conv.return_address).ok()?;
    let mut remembered = Vec::new();
    for block in func.blocks() {
        for inst in func.insts(block) {
            if inst == call {
                return (reg == sp).then_some(offset);
            }
            for op in func.cfi_after(inst) {
                match op {
                    mir::CfiOp::DefCfa { reg: to, offset: by } => (reg, offset) = (to, by),
                    mir::CfiOp::DefCfaOffset(by) => offset = by,
                    mir::CfiOp::DefCfaRegister(to) => reg = to,
                    mir::CfiOp::RememberState => remembered.push((reg, offset)),
                    mir::CfiOp::RestoreState => {
                        if let Some(state) = remembered.pop() {
                            (reg, offset) = state;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    None
}

/// The stores taken out, the pad of `pad` bytes when there are any, a push of each in front of the
/// call from the top word down, and the area and the pad given back behind it. Gives back the
/// `subl` and the `addl` it wrote, which write the stack pointer. See [`padding`] for the pad.
#[allow(clippy::too_many_arguments)]
fn rewrite(
    func: &mut mir::Func,
    call: mir::Inst,
    area: u32,
    pad: u32,
    plan: &Plan,
    ops: &Ops,
    conv: &CallRegs,
    cfa: Option<i32>,
) -> Vec<mir::Inst> {
    let sp = mir::Reg::physical(conv.stack_pointer);
    let class = conv.int_class;
    let mut below = cfa;
    let mut written = Vec::new();
    let first = plan.anchors.last().copied().unwrap_or(call);
    if let Pad::Pushes(register, words) = plan.fill {
        let span = func.span(call);
        for _ in 0..words {
            let push = match register {
                Some(source) => func.build_loose(ops.push).at(span).uses(source.reg, source.class),
                None => func.build_loose(ops.push_imm).at(span).imm(0),
            }
            .finish();
            func.insert_before(first, push);
            below = below.map(|offset| offset + 4);
            if let Some(offset) = below {
                func.cfi.push((push, mir::CfiOp::DefCfaOffset(offset)));
            }
        }
    } else if pad != 0 {
        let span = func.span(call);
        let sub = func
            .build_loose(ops.sub)
            .at(span)
            .def(sp, class)
            .uses(sp, class)
            .imm(i64::from(pad))
            .finish();
        func.insert_before(first, sub);
        written.push(sub);
        below = below.and_then(|offset| offset.checked_add(i32::try_from(pad).ok()?));
        if let Some(offset) = below {
            func.cfi.push((sub, mir::CfiOp::DefCfaOffset(offset)));
        }
    }
    for (&store, &anchor) in plan.stores.iter().zip(&plan.anchors).rev() {
        let data = func[store];
        let span = func.span(store);
        let push = if data.opcode == ops.store {
            let source = func[data.operands][0];
            func.build_loose(ops.push).at(span).uses(source.reg, source.class).finish()
        } else {
            let imm = data.imm.map_or(0, |imm| func[imm].0);
            let build = func.build_loose(ops.push_imm).at(span).imm(imm);
            match data.symbol {
                Some(symbol) => build.symbol(symbol).finish(),
                None => build.finish(),
            }
        };
        func.insert_before(anchor, push);
        below = below.map(|offset| offset + 4);
        if let Some(offset) = below {
            func.cfi.push((push, mir::CfiOp::DefCfaOffset(offset)));
        }
    }
    // Only now, since a push can go in front of the store of a word under it.
    for &store in &plan.stores {
        func.remove_inst(store);
    }
    for &(mem, disp) in &plan.moved {
        func[mem].disp = disp;
    }
    let back = match plan.popped {
        Some(sub) => {
            func.remove_inst(sub);
            call
        }
        None => {
            let span = func.span(call);
            let add = func
                .build_loose(ops.add)
                .at(span)
                .def(sp, class)
                .uses(sp, class)
                .imm(i64::from(area + pad))
                .finish();
            func.insert_after(call, add);
            written.push(add);
            add
        }
    };
    if let Some(offset) = cfa {
        func.cfi.push((back, mir::CfiOp::DefCfaOffset(offset)));
    }
    written
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_mir::{CfiOp, Func, Inst, Mem, Opcode, Operand, Reg};
    use rucc_target::CallRegs;
    use rucc_target::x86::{EAX, ECX, ESP, FRAME, GPR, MACHINE, REGS, SYSV};

    use super::{arguments, frameless, unframed};

    /// The opcode of that name on this target.
    fn op(names: &mut Interner, name: &str) -> Opcode {
        Opcode::new(names.intern(&format!("x64.{name}")))
    }

    fn sp() -> Operand {
        Operand::read(Reg::physical(ESP), GPR)
    }

    /// i386 System V under `-mpreferred-stack-boundary=2`, which is what boot code is built with.
    fn words() -> &'static CallRegs {
        SYSV.aligned_to(4)
    }

    /// `printf("%d", x)` as the lowering leaves it: the address of the format into the bottom word
    /// of the area, the number in `%eax` into the next, whatever `between` puts in front of the
    /// call, and the call. Gives back the function and the call.
    fn printf(
        names: &mut Interner,
        between: impl FnOnce(&mut Func, rucc_mir::Block, &mut Interner),
    ) -> (Func, Inst) {
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        let format = names.intern(".LC0");
        func.build(block, op(names, "mov_mi_32")).mem(Mem::at(sp())).imm(0).symbol(format).finish();
        func.build(block, op(names, "mov_mr_32"))
            .uses(Reg::physical(EAX), GPR)
            .mem(Mem::at(sp()).plus(4))
            .finish();
        between(&mut func, block, names);
        let printf = names.intern("printf");
        // A call names the stack pointer as an operand, as the lowering leaves it.
        let call = func
            .build(block, op(names, "call"))
            .uses(Reg::physical(ESP), GPR)
            .symbol(printf)
            .finish();
        func.build(block, op(names, "ret")).finish();
        (func, call)
    }

    /// The function once rewritten, one instruction to a line from its opcode on, and how many
    /// calls were rewritten.
    fn pushed(
        names: &mut Interner,
        func: &mut Func,
        calls: &[(Inst, u32)],
        conv: &CallRegs,
    ) -> (usize, Vec<String>) {
        let count = arguments(func, calls, &FRAME, &MACHINE, conv, names).calls;
        let lines = rucc_mir::print_func(func, names, &REGS)
            .lines()
            .filter_map(|line| line.find("x64.").map(|at| line[at..].to_owned()))
            .collect();
        (count, lines)
    }

    #[test]
    fn the_arguments_of_a_call_are_pushed_last_first_and_taken_off_after_it() {
        let mut names = Interner::new();
        let (mut func, call) = printf(&mut names, |_, _, _| {});
        let (count, lines) = pushed(&mut names, &mut func, &[(call, 8)], words());
        assert_eq!(count, 1, "{lines:#?}");
        assert_eq!(lines.len(), 5, "{lines:#?}");
        assert!(lines[0].starts_with("x64.push_32"), "{lines:#?}");
        assert!(lines[1].starts_with("x64.push_i_32") && lines[1].contains(".LC0"), "{lines:#?}");
        assert!(lines[2].starts_with("x64.call"), "{lines:#?}");
        assert!(lines[3].starts_with("x64.add_ri_32") && lines[3].contains('8'), "{lines:#?}");
    }

    #[test]
    fn a_callee_that_takes_its_arguments_off_keeps_the_stack_pointer_where_the_pushes_left_it() {
        let mut names = Interner::new();
        let (mut func, call) = printf(&mut names, |_, _, _| {});
        let sub = op(&mut names, "sub_ri_32");
        let esp = Reg::physical(ESP);
        let back = func.build_loose(sub).def(esp, GPR).uses(esp, GPR).imm(8).finish();
        func.insert_after(call, back);
        let (count, lines) = pushed(&mut names, &mut func, &[(call, 8)], words());
        assert_eq!(count, 1, "{lines:#?}");
        assert!(lines.iter().all(|line| !line.contains("sub_ri") && !line.contains("add_ri")));
        assert!(lines[2].starts_with("x64.call"), "{lines:#?}");
    }

    #[test]
    fn a_register_written_after_its_store_is_pushed_in_front_of_the_write() {
        let mut names = Interner::new();
        let (mut func, call) = printf(&mut names, |func, block, names| {
            func.build(block, op(names, "mov_ri_32")).def(Reg::physical(EAX), GPR).imm(1).finish();
        });
        let (count, lines) = pushed(&mut names, &mut func, &[(call, 8)], words());
        assert_eq!(count, 1, "{lines:#?}");
        assert!(lines[0].starts_with("x64.push_32"), "{lines:#?}");
        assert!(lines[1].starts_with("x64.mov_ri_32"), "{lines:#?}");
        assert!(lines[2].starts_with("x64.push_i_32") && lines[2].contains(".LC0"), "{lines:#?}");
        assert!(lines[3].starts_with("x64.call"), "{lines:#?}");
    }

    #[test]
    fn a_slot_read_behind_a_push_is_read_from_as_far_above_it() {
        let mut names = Interner::new();
        let (mut func, call) = printf(&mut names, |func, block, names| {
            func.build(block, op(names, "mov_ri_32")).def(Reg::physical(EAX), GPR).imm(1).finish();
            func.build(block, op(names, "mov_rm_32"))
                .def(Reg::physical(ECX), GPR)
                .mem(Mem::at(sp()).plus(12))
                .finish();
        });
        let (count, lines) = pushed(&mut names, &mut func, &[(call, 8)], words());
        assert_eq!(count, 1, "{lines:#?}");
        assert!(lines[2].starts_with("x64.mov_rm_32") && lines[2].contains("16"), "{lines:#?}");
        assert!(!lines[2].contains("12"), "{lines:#?}");
    }

    #[test]
    fn two_words_from_one_register_written_between_their_stores_are_left_alone() {
        // The bottom word would have to be pushed before the top one.
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        let eax = Reg::physical(EAX);
        func.build(block, op(&mut names, "mov_mr_32")).uses(eax, GPR).mem(Mem::at(sp())).finish();
        func.build(block, op(&mut names, "mov_ri_32")).def(eax, GPR).imm(1).finish();
        func.build(block, op(&mut names, "mov_mr_32"))
            .uses(eax, GPR)
            .mem(Mem::at(sp()).plus(4))
            .finish();
        let callee = names.intern("g");
        let call = func.build(block, op(&mut names, "call")).symbol(callee).finish();
        func.build(block, op(&mut names, "ret")).finish();
        let (count, lines) = pushed(&mut names, &mut func, &[(call, 8)], words());
        assert_eq!(count, 0, "{lines:#?}");
        assert!(lines.iter().all(|line| !line.contains("push")), "{lines:#?}");
    }

    /// A function that takes its arguments in registers under `-mregparm=3` and passes them on:
    /// each arrives through an `arg_val_32`, which writes the register and encodes to nothing.
    #[test]
    fn the_arrival_of_an_argument_between_the_stores_is_no_obstacle() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        let format = names.intern(".LC0");
        func.build(block, op(&mut names, "mov_mi_32"))
            .mem(Mem::at(sp()))
            .imm(0)
            .symbol(format)
            .finish();
        for (reg, at) in [(EAX, 4), (ECX, 8)] {
            func.build(block, op(&mut names, "arg_val_32")).def(Reg::physical(reg), GPR).finish();
            func.build(block, op(&mut names, "mov_mr_32"))
                .uses(Reg::physical(reg), GPR)
                .mem(Mem::at(sp()).plus(at))
                .finish();
        }
        let printf = names.intern("printf");
        let call = func.build(block, op(&mut names, "call")).symbol(printf).finish();
        func.build(block, op(&mut names, "ret")).finish();
        let (count, lines) = pushed(&mut names, &mut func, &[(call, 12)], words());
        assert_eq!(count, 1, "{lines:#?}");
        assert_eq!(lines.iter().filter(|line| line.contains("push")).count(), 3, "{lines:#?}");
    }

    #[test]
    fn a_word_of_the_area_read_before_the_call_keeps_its_store() {
        let mut names = Interner::new();
        let (mut func, call) = printf(&mut names, |func, block, names| {
            func.build(block, op(names, "mov_rm_32"))
                .def(Reg::physical(ECX), GPR)
                .mem(Mem::at(sp()).plus(4))
                .finish();
        });
        let (count, _) = pushed(&mut names, &mut func, &[(call, 8)], words());
        assert_eq!(count, 0);
    }

    #[test]
    fn a_slot_above_the_area_read_before_the_call_is_no_obstacle() {
        let mut names = Interner::new();
        let (mut func, call) = printf(&mut names, |func, block, names| {
            func.build(block, op(names, "mov_rm_32"))
                .def(Reg::physical(ECX), GPR)
                .mem(Mem::at(sp()).plus(12))
                .finish();
        });
        let (count, _) = pushed(&mut names, &mut func, &[(call, 8)], words());
        assert_eq!(count, 1);
    }

    #[test]
    fn an_area_whose_stores_are_not_all_there_is_left_alone() {
        let mut names = Interner::new();
        let (mut func, call) = printf(&mut names, |_, _, _| {});
        let (count, _) = pushed(&mut names, &mut func, &[(call, 12)], words());
        assert_eq!(count, 0);
    }

    #[test]
    fn a_call_through_a_word_counted_from_the_stack_pointer_keeps_its_stores() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        func.build(block, op(&mut names, "mov_mr_32"))
            .uses(Reg::physical(EAX), GPR)
            .mem(Mem::at(sp()))
            .finish();
        let call =
            func.build(block, op(&mut names, "call_mem")).mem(Mem::at(sp()).plus(8)).finish();
        func.build(block, op(&mut names, "ret")).finish();
        let (count, lines) = pushed(&mut names, &mut func, &[(call, 4)], words());
        assert_eq!(count, 0, "{lines:#?}");
    }

    #[test]
    fn a_call_on_a_sixteen_byte_boundary_is_padded_to_it() {
        let mut names = Interner::new();
        let (mut func, call) = printf(&mut names, |func, block, names| {
            func.build(block, op(names, "mov_mr_32"))
                .uses(Reg::physical(ECX), GPR)
                .mem(Mem::at(sp()).plus(8))
                .finish();
        });
        let (count, lines) = pushed(&mut names, &mut func, &[(call, 12)], &SYSV);
        assert_eq!(count, 1, "{lines:#?}");
        assert!(lines[0].starts_with("x64.push_32"), "{lines:#?}");
        assert_eq!(lines[0], lines[1], "the pad is a push of the top argument's register");
        assert!(lines[4].starts_with("x64.call"), "{lines:#?}");
        assert!(lines[5].starts_with("x64.add_ri_32") && lines[5].contains("16"), "{lines:#?}");
    }

    #[test]
    fn a_pad_of_two_words_is_two_pushes_of_a_register_and_with_none_a_sub() {
        // `f(5, x)` and `f(5, 6)`, which pass eight bytes under a sixteen byte boundary.
        let call = |names: &mut Interner, register: bool| {
            let mut func = Func::new(names.intern("f"));
            let block = func.create_block();
            func.build(block, op(names, "mov_mi_32")).mem(Mem::at(sp())).imm(5).finish();
            let top =
                func.build(block, op(names, if register { "mov_mr_32" } else { "mov_mi_32" }));
            let top = if register { top.uses(Reg::physical(EAX), GPR) } else { top.imm(6) };
            top.mem(Mem::at(sp()).plus(4)).finish();
            let call = func.build(block, op(names, "call")).uses(Reg::physical(ESP), GPR).finish();
            func.build(block, op(names, "ret")).finish();
            (func, call)
        };
        let mut names = Interner::new();
        let (mut func, at) = call(&mut names, true);
        let (count, lines) = pushed(&mut names, &mut func, &[(at, 8)], &SYSV);
        assert_eq!(count, 1, "{lines:#?}");
        assert!(lines[0].starts_with("x64.push_32"), "{lines:#?}");
        assert_eq!(lines[0], lines[1], "{lines:#?}");
        assert_eq!(lines[1], lines[2], "the pad pushes the register of the top word: {lines:#?}");
        assert!(lines[4].starts_with("x64.call"), "{lines:#?}");
        let (mut func, at) = call(&mut names, false);
        let (count, lines) = pushed(&mut names, &mut func, &[(at, 8)], &SYSV);
        assert_eq!(count, 1, "{lines:#?}");
        assert!(lines[0].starts_with("x64.sub_ri_32") && lines[0].contains('8'), "{lines:#?}");
        assert!(lines[3].starts_with("x64.call"), "{lines:#?}");
    }

    #[test]
    fn a_padded_call_that_is_not_shorter_keeps_its_stores() {
        let mut names = Interner::new();
        let (mut func, call) = printf(&mut names, |_, _, _| {});
        let (count, _) = pushed(&mut names, &mut func, &[(call, 8)], &SYSV);
        assert_eq!(count, 0);
    }

    #[test]
    fn a_padded_call_whose_callee_takes_its_arguments_off_keeps_its_stores() {
        let mut names = Interner::new();
        let (mut func, call) = printf(&mut names, |_, _, _| {});
        let sub = op(&mut names, "sub_ri_32");
        let esp = Reg::physical(ESP);
        let back = func.build_loose(sub).def(esp, GPR).uses(esp, GPR).imm(8).finish();
        func.insert_after(call, back);
        let (count, _) = pushed(&mut names, &mut func, &[(call, 8)], &SYSV);
        assert_eq!(count, 0);
    }

    #[test]
    fn one_register_argument_is_shorter_stored() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        func.build(block, op(&mut names, "mov_mr_32"))
            .uses(Reg::physical(EAX), GPR)
            .mem(Mem::at(sp()))
            .finish();
        let callee = names.intern("g");
        let call = func.build(block, op(&mut names, "call")).symbol(callee).finish();
        let (count, _) = pushed(&mut names, &mut func, &[(call, 4)], words());
        assert_eq!(count, 0);
    }

    #[test]
    fn the_unwind_table_follows_each_push_and_the_way_back() {
        let mut names = Interner::new();
        let (mut func, call) = printf(&mut names, |_, _, _| {});
        let first = func.insts(func.blocks().next().expect("a block")).next().expect("a store");
        let sub = op(&mut names, "sub_ri_32");
        let esp = Reg::physical(ESP);
        let frame = func.build_loose(sub).def(esp, GPR).uses(esp, GPR).imm(12).finish();
        func.insert_before(first, frame);
        func.cfi.push((frame, CfiOp::DefCfaOffset(16)));
        let (count, _) = pushed(&mut names, &mut func, &[(call, 8)], words());
        assert_eq!(count, 1);
        let rows: Vec<CfiOp> = func.cfi.iter().map(|&(_, row)| row).collect();
        assert_eq!(
            rows,
            [16, 20, 24, 16].map(CfiOp::DefCfaOffset),
            "one row for each push and one behind the call"
        );
        let back = func.next_inst(call).expect("the add");
        assert!(func.cfi_after(back).eq([CfiOp::DefCfaOffset(16)]));
    }

    /// `printf("%d", x)` in a frame of `size` bytes whose bottom `outgoing` are the area, with the
    /// prologue's `subl` in front, the epilogue's `addl` behind and a read of the word at `read` in
    /// front of the stores, and the unwind rows of the two. An area of 12 bytes also stores `%ecx`
    /// in the third word. Pushed and then unframed with calls aligned to `align` bytes, one
    /// instruction to a line from the opcode on, and the offsets the rows are left with.
    fn framed(size: i64, outgoing: u32, read: i32, align: u32) -> (bool, Vec<String>, Vec<i32>) {
        let mut names = Interner::new();
        let (mut func, call) = printf(&mut names, |func, block, names| {
            // A third word, so the area is not a multiple of a 16-byte alignment.
            if outgoing > 8 {
                func.build(block, op(names, "mov_mr_32"))
                    .uses(Reg::physical(ECX), GPR)
                    .mem(Mem::at(sp()).plus(8))
                    .finish();
            }
        });
        let block = func.blocks().next().expect("a block");
        let first = func.insts(block).next().expect("a store");
        let ret = func.insts(block).last().expect("the return");
        let esp = Reg::physical(ESP);
        let sub = op(&mut names, "sub_ri_32");
        let frame = func.build_loose(sub).def(esp, GPR).uses(esp, GPR).imm(size).finish();
        func.insert_before(first, frame);
        let bytes = i32::try_from(size).expect("a size");
        func.cfi.push((frame, CfiOp::DefCfaOffset(4 + bytes)));
        let load = op(&mut names, "mov_rm_32");
        let read =
            func.build_loose(load).def(Reg::physical(EAX), GPR).mem(Mem::at(sp()).plus(read));
        let read = read.finish();
        func.insert_before(first, read);
        let add = op(&mut names, "add_ri_32");
        let back = func.build_loose(add).def(esp, GPR).uses(esp, GPR).imm(size).finish();
        func.insert_before(ret, back);
        func.cfi.push((back, CfiOp::DefCfaOffset(4)));
        let conv = SYSV.aligned_to(align);
        let pushed = arguments(&mut func, &[(call, outgoing)], &FRAME, &MACHINE, conv, &mut names);
        let size = u32::try_from(size).expect("a size");
        let gone = unframed(&mut func, size, outgoing, &pushed, &FRAME, &MACHINE, conv, &mut names);
        let lines = rucc_mir::print_func(&func, &names, &REGS)
            .lines()
            .filter_map(|line| line.find("x64.").map(|at| line[at..].to_owned()))
            .collect();
        let mut rows: Vec<i32> = func
            .cfi
            .iter()
            .filter_map(|(_, row)| match row {
                CfiOp::DefCfaOffset(offset) => Some(*offset),
                _ => None,
            })
            .collect();
        rows.sort_unstable();
        (gone, lines, rows)
    }

    #[test]
    fn a_frame_that_was_only_the_outgoing_area_goes_once_the_arguments_are_pushed() {
        let (gone, lines, rows) = framed(8, 8, 16, 4);
        assert!(gone, "{lines:#?}");
        // The two pushes and the way back, each eight bytes nearer, and no row for the frame.
        assert_eq!(rows, [4, 8, 12], "{lines:#?}");
        assert!(!lines.iter().any(|line| line.starts_with("x64.sub_ri_32")), "{lines:#?}");
        let adds = lines.iter().filter(|line| line.starts_with("x64.add_ri_32")).count();
        assert_eq!(adds, 1, "only the one behind the call is left\n{lines:#?}");
        assert!(lines[0].starts_with("x64.mov_rm_32") && lines[0].contains("+ 8]"), "{lines:#?}");
    }

    #[test]
    fn a_frame_that_was_the_outgoing_area_and_its_pad_goes_once_the_arguments_are_pushed() {
        let (gone, lines, _) = framed(16, 12, 16, 16);
        assert!(gone, "{lines:#?}");
        assert!(!lines.iter().any(|line| line.starts_with("x64.sub_ri_32")), "{lines:#?}");
    }

    #[test]
    fn a_frame_larger_than_the_outgoing_area_and_its_pad_stays() {
        let (gone, lines, _) = framed(32, 12, 32, 16);
        assert!(!gone, "{lines:#?}");
        assert!(lines.iter().any(|line| line.starts_with("x64.sub_ri_32")), "{lines:#?}");
    }

    /// `printf("%d", x)` in a frame of `size` bytes whose bottom 8 are the area, with the prologue's
    /// `subl` in front, the epilogue's `addl` behind, a read of the word at `read` in front of the
    /// stores, and a call of `abort` with nothing on the stack behind the first call when `abort` is
    /// set. Made frameless with calls aligned to 16 bytes, one instruction to a line from the
    /// opcode on.
    fn unpadded(size: i64, read: i32, abort: bool) -> (bool, Vec<String>) {
        let mut names = Interner::new();
        let (mut func, call) = printf(&mut names, |_, _, _| {});
        let block = func.blocks().next().expect("a block");
        let first = func.insts(block).next().expect("a store");
        let ret = func.insts(block).last().expect("the return");
        let esp = Reg::physical(ESP);
        let sub = op(&mut names, "sub_ri_32");
        let frame = func.build_loose(sub).def(esp, GPR).uses(esp, GPR).imm(size).finish();
        func.insert_before(first, frame);
        let bytes = i32::try_from(size).expect("a size");
        func.cfi.push((frame, CfiOp::DefCfaOffset(4 + bytes)));
        let load = op(&mut names, "mov_rm_32");
        let read =
            func.build_loose(load).def(Reg::physical(EAX), GPR).mem(Mem::at(sp()).plus(read));
        let read = read.finish();
        func.insert_before(first, read);
        if abort {
            let callee = names.intern("abort");
            let other = func.build_loose(op(&mut names, "call")).uses(esp, GPR).symbol(callee);
            let other = other.finish();
            func.insert_before(ret, other);
        }
        let add = op(&mut names, "add_ri_32");
        let back = func.build_loose(add).def(esp, GPR).uses(esp, GPR).imm(size).finish();
        func.insert_before(ret, back);
        func.cfi.push((back, CfiOp::DefCfaOffset(4)));
        let size = u32::try_from(size).expect("a size");
        let conv = SYSV.aligned_to(16);
        let calls = [(call, 8)];
        let gone = frameless(&mut func, size, 8, &calls, &FRAME, &MACHINE, conv, &mut names);
        let lines = rucc_mir::print_func(&func, &names, &REGS)
            .lines()
            .filter_map(|line| line.find("x64.").map(|at| line[at..].to_owned()))
            .collect();
        (gone, lines)
    }

    #[test]
    fn a_frame_that_aligns_the_outgoing_area_with_the_return_address_goes() {
        let (gone, lines) = unpadded(12, 12, false);
        assert!(gone, "{lines:#?}");
        let expected = [
            "x64.mov_rm_32 [$esp]",
            "x64.push_32",
            "x64.push_32",
            "x64.push_i_32",
            "x64.call",
            "x64.add_ri_32 $esp, 12",
            "x64.ret",
        ];
        assert_eq!(lines.len(), expected.len(), "{lines:#?}");
        for (line, start) in lines.iter().zip(expected) {
            assert!(line.starts_with(start), "{line} is not {start}: {lines:#?}");
        }
    }

    #[test]
    fn a_frame_with_a_call_that_passes_nothing_on_the_stack_stays() {
        let (gone, lines) = unpadded(12, 12, true);
        assert!(!gone, "{lines:#?}");
        assert!(lines[0].starts_with("x64.sub_ri_32"), "{lines:#?}");
    }

    #[test]
    fn a_frame_with_a_word_read_above_the_area_stays() {
        let (gone, lines) = unpadded(12, 8, false);
        assert!(!gone, "{lines:#?}");
        assert!(lines[0].starts_with("x64.sub_ri_32"), "{lines:#?}");
    }

    #[test]
    fn a_frame_with_more_than_the_outgoing_area_in_it_stays() {
        let (gone, lines, _) = framed(12, 8, 8, 4);
        assert!(!gone, "{lines:#?}");
        assert!(lines.iter().any(|line| line.starts_with("x64.sub_ri_32")), "{lines:#?}");
    }

    #[test]
    fn a_frame_whose_area_is_still_read_stays() {
        let (gone, lines, _) = framed(8, 8, 4, 4);
        assert!(!gone, "{lines:#?}");
        assert!(lines.iter().any(|line| line.starts_with("x64.sub_ri_32")), "{lines:#?}");
    }
}
