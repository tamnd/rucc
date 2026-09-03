//! The frame: what a function's stack looks like while it runs.
//!
//! Design: `spec/10-backend.md` section 10.7.
//!
//! This is worked out after register allocation and not before, because the largest area in most
//! frames is the spill slots and nothing knows how many of those there are until the allocator has
//! finished running out of registers. It is worked out from the rewritten function rather than
//! from the assignment alone, because the rewrite is what decides which scratch registers a reload
//! uses, and a scratch register a call preserves is one the prologue has to save.
//!
//! # What is in one
//!
//! Section 10.7 lists the areas and this is the order they are in, from the stack pointer upward,
//! which is the order of increasing address on every machine here.
//!
//! ```text
//!   incoming stack arguments      the caller wrote these and they are above everything
//!   return address                the call instruction pushed it, on a machine that does
//!   saved frame pointer           when the function keeps one
//!   saved general purpose regs    pushed, one word each
//!   saved vector registers        stored rather than pushed, since no machine here pushes one
//!   locals                        what an alloca becomes, widest alignment first
//!   spill slots                   one for every value the allocator ran out of registers for
//!   outgoing argument area        at the bottom, because a call reads its stack arguments from
//!                                 the stack pointer upward
//! ```
//!
//! Every offset reported here is from the stack pointer as it stands in the body of the function,
//! which is after the prologue and before the epilogue. That is the one base register always
//! available. A frame pointer is a second way to reach the same bytes and the prologue is what
//! knows the distance between the two, so nothing here reports an offset from it.
//!
//! # Where the alignment comes from
//!
//! A call has to leave the stack pointer on a multiple of the convention's alignment, so a
//! function's own frame is what puts it back: the call that reached this function pushed a return
//! address and left the stack pointer one word off, and the prologue's pushes either fix that or
//! make it worse depending on how many there are. The size the prologue subtracts is therefore not
//! the size of the areas. It is whatever brings the stack pointer back to a multiple of the
//! alignment given the pushes in front of it, which is the arithmetic in [`Frame::of`].
//!
//! # The red zone
//!
//! A leaf function may use the bytes below the stack pointer without moving it, which is what
//! `red_zone` on a convention says and what makes a small leaf function's prologue and epilogue
//! empty. Then the offsets are negative, which is why they are signed, and the areas are in the
//! same order as ever, below the line rather than above it. Anything that calls, or is too big for
//! the zone, or wants more alignment than the stack pointer has for free, moves the stack pointer.
//!
//! # Realignment
//!
//! A local wanting more alignment than a call leaves the stack pointer with cannot be placed by
//! arithmetic, because nothing in the frame knows what the caller's stack pointer was a multiple
//! of. The prologue has to force it, and forcing it destroys the only record of where the caller's
//! stack was, so a realigned frame needs a frame pointer and the distance from the body's stack
//! pointer to the incoming arguments stops being a constant. [`Frame::realign`] is where that is
//! reported and it is why [`Frame::incoming`] can answer that it does not know.

use rucc_mir::Func;
use rucc_regalloc::Allocation;
use rucc_regalloc::assign::Place;
use rucc_target::{CallRegs, PhysReg, RegClass, RegFile};

/// A piece of memory the function needs for its own use, which is what an `alloca` becomes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Local {
    /// How many bytes of it there are.
    pub size: u32,
    /// What its address has to be a multiple of.
    pub align: u32,
}

/// Everything about a function's frame that does not come out of its allocation.
#[derive(Debug, Clone, Copy)]
pub struct Layout<'a> {
    /// Where the convention this function is compiled for puts things.
    pub conv: &'a CallRegs,
    /// The registers the target has, which is what says how wide a spill slot of a class is.
    pub file: RegFile,
    /// The memory the function asked for itself, in the order it wants it reported back.
    pub locals: &'a [Local],
    /// How many bytes the widest call in the function needs for arguments it passes on the stack.
    pub outgoing: u32,
    /// Whether the function calls nothing, which is what the alignment and the red zone turn on.
    pub leaf: bool,
    /// Whether the function keeps a frame pointer, which `-fno-omit-frame-pointer` asks for and
    /// which a realigned or a dynamically grown frame requires whatever the flags say.
    pub frame_pointer: bool,
    /// Whether the red zone may be used at all, which `-mno-red-zone` and every kernel turns off.
    pub red_zone: bool,
}

impl<'a> Layout<'a> {
    /// A layout for a function with nothing in it but what its allocation says: a leaf with no
    /// locals and no calls, which is what every function is until the pieces that produce those
    /// exist.
    #[must_use]
    pub fn new(conv: &'a CallRegs, file: RegFile) -> Self {
        Self {
            conv,
            file,
            locals: &[],
            outgoing: 0,
            leaf: true,
            frame_pointer: false,
            red_zone: true,
        }
    }
}

/// What a function's stack looks like while it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    saved_int: Vec<PhysReg>,
    saved_sse: Vec<PhysReg>,
    slots: Vec<i32>,
    locals: Vec<i32>,
    outgoing: u32,
    size: u32,
    realign: Option<u32>,
    incoming: Option<i32>,
    frame_pointer: bool,
}

impl Frame {
    /// Works out the frame of a function the allocator has finished with.
    ///
    /// # Panics
    ///
    /// Panics on a frame of two gigabytes or more, which is a stack no machine here gives a
    /// thread, and on a local whose alignment is not a power of two.
    #[must_use]
    pub fn of(func: &Func, allocation: &Allocation, layout: &Layout<'_>) -> Self {
        let conv = layout.conv;
        let word = conv.word;
        let (saved_int, saved_sse) = saved(func, allocation, layout);

        // The vector registers are saved in the frame rather than pushed, because no machine here
        // has an instruction that pushes one.
        let vector = width(layout, conv.sse_class);
        let mut top = u32::try_from(saved_sse.len()).expect("a frame") * vector;
        let mut align = if saved_sse.is_empty() { word } else { vector };

        let mut locals = vec![0; layout.locals.len()];
        let mut order: Vec<usize> = (0..layout.locals.len()).collect();
        // Widest alignment first, so that placing each one straight after the last never leaves a
        // hole bigger than the alignment the next one asked for.
        order.sort_by_key(|&local| std::cmp::Reverse(layout.locals[local].align));
        for local in order {
            let Local { size, align: want } = layout.locals[local];
            assert!(
                want.is_power_of_two(),
                "a local aligned to something that is not a power of 2"
            );
            align = align.max(want);
            top = top.next_multiple_of(want);
            locals[local] = offset(top);
            top += size;
        }

        let mut slots = Vec::with_capacity(allocation.assignment.slots().len());
        for &class in allocation.assignment.slots() {
            let size = width(layout, class);
            align = align.max(size);
            top = top.next_multiple_of(size);
            slots.push(offset(top));
            top += size;
        }

        // A call reads its stack arguments from the stack pointer upward, so the outgoing area is
        // at the bottom of the frame and its size is what shifts everything else.
        let outgoing = if layout.leaf { 0 } else { layout.outgoing.max(conv.shadow) };
        let body = (top + outgoing).next_multiple_of(word);

        // Where the stack pointer sits once the prologue has finished pushing: one return address
        // short of aligned when the function starts, and one word further off for every push.
        let pushed =
            u32::from(layout.frame_pointer) + u32::try_from(saved_int.len()).expect("a frame");
        let entry = wrap(conv.stack_align, conv.return_address);
        let after = (entry + wrap(conv.stack_align, word * pushed)) % conv.stack_align;

        let realign = (align > conv.stack_align).then_some(align);
        let free = layout.leaf
            && layout.red_zone
            && realign.is_none()
            && align <= word
            && body <= conv.red_zone;
        let size = match realign {
            _ if free => 0,
            // Once the prologue has forced the alignment, keeping the frame a multiple of it keeps
            // everything in the frame aligned too.
            Some(to) => body.next_multiple_of(to),
            // A leaf owes nobody an aligned stack pointer, so it takes exactly what it uses.
            None if layout.leaf && align <= word => body,
            // The smallest frame that lands the stack pointer back on a multiple of the alignment
            // given where the pushes left it.
            None => body + (after + conv.stack_align - body % conv.stack_align) % conv.stack_align,
        };

        // With the stack pointer left where it was, the areas are the same areas in the same order
        // and they are below it rather than above it.
        let shift = if free { -offset(body) } else { offset(outgoing) };
        for at in slots.iter_mut().chain(locals.iter_mut()) {
            *at += shift;
        }

        Self {
            saved_int,
            saved_sse,
            slots,
            locals,
            outgoing,
            size,
            realign,
            incoming: realign.is_none().then(|| offset(size + word * pushed + conv.return_address)),
            frame_pointer: layout.frame_pointer || realign.is_some(),
        }
    }

    /// The general purpose registers the prologue pushes, in the order it pushes them.
    ///
    /// The frame pointer is not among them even when the convention calls it a saved register,
    /// because a function that keeps one saves it as part of setting it up.
    #[must_use]
    pub fn saved_int(&self) -> &[PhysReg] {
        &self.saved_int
    }

    /// The vector registers the prologue stores into the frame, in the order it stores them.
    #[must_use]
    pub fn saved_sse(&self) -> &[PhysReg] {
        &self.saved_sse
    }

    /// Where a spill slot is, from the stack pointer in the body of the function.
    #[must_use]
    pub fn slot(&self, slot: u32) -> Option<i32> {
        self.slots.get(usize::try_from(slot).ok()?).copied()
    }

    /// Where a local is, from the stack pointer in the body of the function.
    #[must_use]
    pub fn local(&self, local: usize) -> Option<i32> {
        self.locals.get(local).copied()
    }

    /// How many bytes the prologue takes off the stack pointer, which is nothing for a function
    /// small enough and quiet enough to live in the red zone.
    #[must_use]
    pub fn size(&self) -> u32 {
        self.size
    }

    /// How many bytes at the bottom of the frame belong to the arguments of calls this function
    /// makes, which is where the shadow space goes on Windows.
    #[must_use]
    pub fn outgoing(&self) -> u32 {
        self.outgoing
    }

    /// What the prologue has to force the stack pointer to be a multiple of, when a local wants
    /// more alignment than a call leaves it with.
    #[must_use]
    pub fn realign(&self) -> Option<u32> {
        self.realign
    }

    /// Where the first argument the caller passed on the stack is, from the stack pointer in the
    /// body of the function.
    ///
    /// A realigned frame answers that it does not know, because forcing the alignment threw away
    /// however far the caller's stack pointer was from where the prologue wanted it, and the frame
    /// pointer is what reaches the caller's stack afterwards.
    #[must_use]
    pub fn incoming(&self) -> Option<i32> {
        self.incoming
    }

    /// Whether the function keeps a frame pointer.
    #[must_use]
    pub fn frame_pointer(&self) -> bool {
        self.frame_pointer
    }
}

/// The registers a call preserves that this function writes anyway, so the prologue has to put
/// them back.
///
/// The rewritten function is what is read here rather than the assignment, because a spilled value
/// is reloaded into a scratch register that no assignment mentions, and a scratch register the
/// convention preserves is one this has to find.
fn saved(
    func: &Func,
    allocation: &Allocation,
    layout: &Layout<'_>,
) -> (Vec<PhysReg>, Vec<PhysReg>) {
    let mut used: Vec<(RegClass, PhysReg)> = Vec::new();
    let mut note = |class: RegClass, at: PhysReg| {
        if !used.contains(&(class, at)) {
            used.push((class, at));
        }
    };
    for block in func.blocks() {
        for inst in func.insts(block) {
            for operand in &func[func[inst].operands] {
                if let Some(at) = operand.reg.phys() {
                    note(operand.class, at);
                }
            }
        }
    }
    for edit in &allocation.edits {
        for place in [edit.mov.from, edit.mov.to] {
            if let Place::Reg(at) = place {
                note(edit.class, at);
            }
        }
    }

    let conv = layout.conv;
    let wanted = |class: RegClass, at: PhysReg| used.contains(&(class, at));
    // In the convention's order rather than the order the function happened to reach for them, so
    // that two functions saving the same registers get the same prologue.
    let saved_int = conv
        .int_saved
        .iter()
        .copied()
        .filter(|&at| wanted(conv.int_class, at))
        .filter(|&at| !(layout.frame_pointer && at == conv.frame_pointer))
        .collect();
    let saved_sse =
        conv.sse_saved.iter().copied().filter(|&at| wanted(conv.sse_class, at)).collect();
    (saved_int, saved_sse)
}

/// How many bytes a value of a class takes on the stack.
///
/// A power of two at least a word wide, because a slot is addressed and an address that is not a
/// multiple of the size of the thing at it is a fault on some machines and slow on the rest. An
/// eighty bit `long double` takes sixteen bytes for that reason, which is what every compiler
/// does with one.
fn width(layout: &Layout<'_>, class: RegClass) -> u32 {
    let bits = layout.file.class(class).map_or(0, |info| info.bits);
    bits.div_ceil(8).max(layout.conv.word).next_power_of_two()
}

/// How far past a multiple of an alignment a number is, counted the other way: what has to be
/// added to it to reach the next one.
fn wrap(align: u32, value: u32) -> u32 {
    (align - value % align) % align
}

/// A distance in a frame, as the signed number every offset out of here is.
fn offset(bytes: u32) -> i32 {
    i32::try_from(bytes).expect("a frame under two gigabytes")
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_mir::{Opcode, Operand, Reg};
    use rucc_regalloc::assign::Env;
    use rucc_target::x86_64::{GPR, RBP, REGS, SYSV, WIN64, XMM};

    use super::*;

    /// An environment offering that many of the convention's registers, with everything after
    /// them held back as scratch.
    fn env(conv: &CallRegs, count: usize) -> Env {
        Env::new().with(GPR, &conv.int_order[..count], &conv.int_order[count..])
    }

    /// A function of that many values, every one of them written before any is read, allocated
    /// with that many registers to hand out.
    ///
    /// Every value is live at the first read, so a count below the number of values is what puts
    /// the function under enough pressure to spill, and each read wants one value so a reload
    /// never needs more than one scratch register.
    fn pressure(conv: &CallRegs, values: usize, count: usize) -> (Func, Allocation) {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        let regs: Vec<Reg> = (0..values).map(|_| func.new_vreg(GPR)).collect();
        for &reg in &regs {
            func.build(block, opcode).def(reg, GPR).finish();
        }
        for &reg in &regs {
            func.build(block, opcode).uses(reg, GPR).finish();
        }
        let allocation = rucc_regalloc::run(&mut func, &env(conv, count));
        (func, allocation)
    }

    /// What a list of registers is called, which is what an assertion reads.
    fn named(regs: &[PhysReg]) -> Vec<&'static str> {
        regs.iter().map(|&reg| REGS.name(GPR, reg).expect("a register")).collect()
    }

    #[test]
    fn a_function_that_needs_nothing_of_the_stack_has_no_frame_at_all() {
        let (func, allocation) = pressure(&SYSV, 2, 4);
        let frame = Frame::of(&func, &allocation, &Layout::new(&SYSV, REGS));

        assert_eq!(frame.size(), 0);
        assert_eq!(named(frame.saved_int()), Vec::<&str>::new());
        assert_eq!(frame.slot(0), None);
        // Nothing between the stack pointer and the return address the call pushed.
        assert_eq!(frame.incoming(), Some(8));
    }

    #[test]
    fn a_small_leaf_function_puts_its_spills_in_the_red_zone_and_moves_nothing() {
        let (func, allocation) = pressure(&SYSV, 4, 2);
        let frame = Frame::of(&func, &allocation, &Layout::new(&SYSV, REGS));

        // Two registers for four values that are all live at once, so two are on the stack, and a
        // leaf function small enough is entitled to the bytes below the stack pointer.
        assert_eq!(frame.size(), 0);
        assert_eq!((frame.slot(0), frame.slot(1)), (Some(-16), Some(-8)));
        assert_eq!(frame.slot(2), None);
        assert_eq!(frame.incoming(), Some(8));
    }

    #[test]
    fn a_leaf_function_told_it_has_no_red_zone_takes_the_bytes_instead() {
        let (func, allocation) = pressure(&SYSV, 4, 2);
        let base = Layout::new(&SYSV, REGS);
        let frame = Frame::of(&func, &allocation, &Layout { red_zone: false, ..base });

        assert_eq!(frame.size(), 16);
        assert_eq!((frame.slot(0), frame.slot(1)), (Some(0), Some(8)));
        assert_eq!(frame.incoming(), Some(24));
    }

    #[test]
    fn a_frame_too_big_for_the_red_zone_takes_the_bytes_whatever_else_is_true() {
        let (func, allocation) = pressure(&SYSV, 40, 2);
        let frame = Frame::of(&func, &allocation, &Layout::new(&SYSV, REGS));

        // Thirty eight values on the stack is three hundred and four bytes, and the red zone is a
        // hundred and twenty eight.
        assert_eq!(frame.size(), 304);
        assert_eq!(frame.slot(0), Some(0));
        assert_eq!(frame.slot(37), Some(296));
    }

    #[test]
    fn a_function_that_calls_something_leaves_the_stack_pointer_where_a_call_wants_it() {
        let (func, allocation) = pressure(&SYSV, 4, 2);
        let base = Layout::new(&SYSV, REGS);
        let frame = Frame::of(&func, &allocation, &Layout { leaf: false, ..base });

        // Sixteen bytes of spills, and the call that reached this function left the stack pointer
        // eight bytes off, so the frame is eight bytes wider than the spills need and every call
        // this function makes is correctly aligned.
        assert_eq!(frame.size(), 24);
        assert_eq!((frame.slot(0), frame.slot(1)), (Some(0), Some(8)));
        assert_eq!(frame.incoming(), Some(32));
    }

    #[test]
    fn a_push_is_counted_in_the_alignment_the_frame_has_to_produce() {
        let (func, allocation) = pressure(&SYSV, 12, 12);
        let base = Layout::new(&SYSV, REGS);
        let frame = Frame::of(&func, &allocation, &Layout { leaf: false, ..base });

        // Twelve values reach into the preserved end of the allocation order, so three registers
        // are pushed, and three pushes plus the return address is a multiple of sixteen already.
        // The frame is empty and stays empty rather than being padded for the sake of it.
        assert_eq!(named(frame.saved_int()), ["rbx", "r12", "r13"]);
        assert_eq!(frame.size(), 0);
        assert_eq!(frame.incoming(), Some(32));
    }

    #[test]
    fn the_registers_a_call_leaves_alone_are_saved_in_the_order_the_convention_lists_them() {
        let (func, allocation) = pressure(&SYSV, 13, 13);
        let frame = Frame::of(&func, &allocation, &Layout::new(&SYSV, REGS));

        // Four of them now, in the convention's order rather than the order the allocator handed
        // them out in, so that two functions saving the same registers get the same prologue.
        assert_eq!(named(frame.saved_int()), ["rbx", "r12", "r13", "r14"]);
    }

    #[test]
    fn a_function_that_keeps_a_frame_pointer_does_not_save_it_twice() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let opcode = Opcode::new(names.intern("x64.nop"));
        let block = func.create_block();
        // An instruction that names the frame pointer register outright, which is what a lowering
        // rule for something that has to use it produces.
        func.build(block, opcode).operand(Operand::write(Reg::physical(RBP), GPR)).finish();
        let allocation = rucc_regalloc::run(&mut func, &env(&SYSV, 4));
        let base = Layout::new(&SYSV, REGS);

        let kept = Frame::of(&func, &allocation, &Layout { frame_pointer: true, ..base });
        let dropped = Frame::of(&func, &allocation, &base);

        // `rbp` is a register SysV preserves, so a function that leaves it alone saves it in the
        // ordinary way, and a function that keeps a frame pointer in it saves it as part of
        // setting the frame pointer up instead.
        assert_eq!(named(dropped.saved_int()), ["rbp"]);
        assert_eq!(named(kept.saved_int()), Vec::<&str>::new());
        assert!(kept.frame_pointer());
    }

    #[test]
    fn locals_are_placed_widest_alignment_first_and_reported_in_the_order_they_arrived() {
        let (func, allocation) = pressure(&SYSV, 2, 4);
        let locals = [
            Local { size: 1, align: 1 },
            Local { size: 16, align: 16 },
            Local { size: 8, align: 8 },
        ];
        let base = Layout::new(&SYSV, REGS);
        let frame = Frame::of(&func, &allocation, &Layout { locals: &locals, ..base });

        // The sixteen byte one is placed first, so nothing is padded to reach it, and the one
        // byte one goes last where the padding after it costs nothing.
        assert_eq!((frame.local(1), frame.local(2), frame.local(0)), (Some(0), Some(16), Some(24)));
        assert_eq!(frame.local(3), None);
        // A local wanting sixteen byte alignment is more than the stack pointer has for free, so
        // the frame is taken rather than the red zone used, and it is padded to keep the local
        // where it was put.
        assert_eq!(frame.size(), 40);
        assert_eq!(frame.realign(), None);
    }

    #[test]
    fn a_local_wanting_more_alignment_than_a_call_gives_makes_the_prologue_force_it() {
        let (func, allocation) = pressure(&SYSV, 2, 4);
        let locals = [Local { size: 64, align: 32 }];
        let base = Layout::new(&SYSV, REGS);
        let frame = Frame::of(&func, &allocation, &Layout { locals: &locals, ..base });

        assert_eq!(frame.realign(), Some(32));
        assert_eq!(frame.local(0), Some(0));
        assert_eq!(frame.size(), 64);
        // Forcing the alignment throws away how far the caller's stack pointer was from where the
        // prologue wanted it, so a frame pointer is needed and the caller's stack is no longer a
        // constant distance away.
        assert!(frame.frame_pointer());
        assert_eq!(frame.incoming(), None);
    }

    #[test]
    fn a_call_reads_its_stack_arguments_from_the_bottom_of_the_frame() {
        let (func, allocation) = pressure(&SYSV, 4, 2);
        let base = Layout::new(&SYSV, REGS);
        let frame = Frame::of(&func, &allocation, &Layout { leaf: false, outgoing: 24, ..base });

        // The outgoing area is at the stack pointer, because that is where the callee will look
        // for it, and the spills sit above it.
        assert_eq!(frame.outgoing(), 24);
        assert_eq!((frame.slot(0), frame.slot(1)), (Some(24), Some(32)));
        assert_eq!(frame.size(), 40);
    }

    #[test]
    fn a_windows_call_gets_the_thirty_two_bytes_below_it_even_when_it_passes_nothing() {
        let (func, allocation) = pressure(&WIN64, 2, 4);
        let base = Layout::new(&WIN64, REGS);
        let frame = Frame::of(&func, &allocation, &Layout { leaf: false, ..base });

        // Windows has no red zone and every caller reserves thirty two bytes below the call for
        // the callee to spill its register arguments into.
        assert_eq!(frame.outgoing(), 32);
        assert_eq!(frame.size(), 40);
        assert_eq!(frame.incoming(), Some(48));
    }

    #[test]
    fn a_slot_is_as_wide_as_the_widest_thing_of_its_class() {
        let base = Layout::new(&SYSV, REGS);

        assert_eq!(width(&base, GPR), 8);
        assert_eq!(width(&base, XMM), 16);
        // A long double is eighty bits and takes sixteen bytes, because an address has to be a
        // multiple of the size of what is at it.
        assert_eq!(width(&base, REGS.class_named("x87").expect("a class")), 16);
    }
}
