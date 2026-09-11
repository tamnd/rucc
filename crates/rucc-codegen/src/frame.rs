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
//!   stack protector canary        when the function has one, above everything a local reaches
//!   locals                        what an alloca becomes, widest alignment first
//!   spill slots                   one for every value the allocator ran out of registers for
//!   outgoing argument area        at the bottom, because a call reads its stack arguments from
//!                                 the stack pointer upward
//! ```
//!
//! Every offset reported here is from the stack pointer as it stands in the body of the function,
//! which is after the prologue and before the epilogue. That is the one base register always
//! available. A frame pointer is a second way to reach the same bytes and the prologue is what
//! knows the distance between the two, so nothing here reports an offset from it. There are two
//! exceptions and [`Frame::incoming`] is one of them, because the bytes it reports are the caller's
//! rather than this function's, which is the one part of the picture a realigned frame loses sight
//! of. It says which register it counted from. The other is a frame that grows, which is the next
//! section and where the stack pointer stops being a base register at all.
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
//! reported and it is why [`Frame::incoming`] answers from the frame pointer in such a frame and
//! from the stack pointer in every other one.
//!
//! # Growing
//!
//! A variable length array is bytes the function takes off the stack pointer where the declaration
//! stands, so in a function that has one the stack pointer is in a different place in the middle of
//! the body than it was at the top of it. Every other offset in the frame was a distance from the
//! stack pointer, and a distance from a register that moves is not a distance, so in a frame like
//! this they are all distances from the frame pointer instead. That is what [`Layout::grows`] says
//! and [`Frame::grows`] reports, and it is why such a frame keeps a frame pointer whatever the
//! flags asked for, the same way a realigned one does and for a version of the same reason.
//!
//! Three other things follow from it. The red zone is gone, because the zone is the bytes below the
//! stack pointer and the first thing an array like this does is move the stack pointer down over
//! them. The frame asks for the convention's alignment even when nothing in it wanted that much, so
//! that the stack pointer is on a multiple of it when the body starts and stays on one as each
//! array rounds its own size up. And the bytes the array hands out start above the outgoing
//! argument area rather than at the stack pointer, because that area stays at the bottom of the
//! frame wherever the bottom has moved to, which is what [`Frame::below`] is for.
//!
//! Realigning and growing together is the one combination that is not here. After the prologue has
//! forced an alignment the distance from the frame pointer to the body's stack pointer is already
//! not a constant, so there is no register left for the rest of the frame to be counted from, and
//! what fixes that is a second pointer held for the purpose. The lowering refuses that pair rather
//! than this guessing at it.

use rucc_mir::Func;
use rucc_regalloc::Allocation;
use rucc_regalloc::assign::Place;
use rucc_target::{CallRegs, PhysReg, RegClass, RegFile};

/// One register the prologue puts away in the frame, and where in the frame it goes.
///
/// A pushed register does not need one of these, because where it goes is wherever the stack
/// pointer had reached, and the epilogue pops them back in the opposite order without having to
/// know. A register that is stored rather than pushed does need one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Save {
    /// The register.
    pub reg: PhysReg,
    /// Where it goes, from the stack pointer in the body of the function.
    pub at: i32,
}

/// Where the arguments the caller passed on the stack are, and which register reaches them.
///
/// Two fields rather than one number because a realigned frame has no constant distance from its
/// stack pointer to the caller's. Forcing the alignment threw that distance away, and the frame
/// pointer is what still reaches the caller's stack afterwards, which is why a realigned frame is
/// made to keep one. So there is always an answer, and which register it is counted from is part of
/// it rather than something the reader is left to work out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Incoming {
    /// How far above that register the first argument passed on the stack is.
    pub at: i32,
    /// Whether the register is the frame pointer rather than the stack pointer.
    pub through_frame_pointer: bool,
}

impl Incoming {
    /// That far above the stack pointer as it stands in the body of the function, which is where
    /// every other offset in a frame is from.
    #[must_use]
    pub fn from_stack(at: i32) -> Self {
        Self { at, through_frame_pointer: false }
    }

    /// That far above the frame pointer, which is the only way a realigned frame reaches back.
    #[must_use]
    pub fn from_frame(at: i32) -> Self {
        Self { at, through_frame_pointer: true }
    }
}

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
    /// Whether the function moves the stack pointer while it runs, which is what a variable length
    /// array does and what the rest of the frame then has to be reached around.
    ///
    /// See `Growing` in the module documentation. A frame like this keeps a frame pointer, takes
    /// its bytes rather than living in the red zone, and reports every offset in its body from the
    /// frame pointer, because the stack pointer stops being somewhere a constant reaches from.
    pub grows: bool,
    /// Whether the red zone may be used at all, which `-mno-red-zone` and every kernel turns off.
    pub red_zone: bool,
    /// Whether the frame holds a stack protector's canary, which `-fstack-protector` and the
    /// function's own attribute decide between them.
    ///
    /// A protected frame is never a leaf, whatever the function called, because the check at the
    /// end of it calls when it fails. The caller sets `leaf` accordingly rather than this working
    /// it out, so that there is one place a frame learns whether it owes an aligned stack pointer.
    pub protect: bool,
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
            grows: false,
            red_zone: true,
            protect: false,
        }
    }
}

/// What a function's stack looks like while it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    saved_int: Vec<PhysReg>,
    saved_sse: Vec<Save>,
    slots: Vec<i32>,
    locals: Vec<i32>,
    canary: Option<i32>,
    outgoing: u32,
    below: u32,
    size: u32,
    realign: Option<u32>,
    incoming: Incoming,
    frame_pointer: bool,
    grows: bool,
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
        let (saved_int, vectors) = saved(func, allocation, layout);

        // The vector registers are saved in the frame rather than pushed, because no machine here
        // has an instruction that pushes one.
        let vector = width(layout, conv.sse_class);
        let mut top = 0;
        let mut align = word;
        let mut saved_sse = Vec::with_capacity(vectors.len());
        for reg in vectors {
            align = align.max(vector);
            saved_sse.push(Save { reg, at: offset(top) });
            top += vector;
        }

        // A frame that grows hands out the bytes above the outgoing area, and what makes that
        // address usable for anything is the stack pointer being on a multiple of the convention's
        // alignment when the body starts. Asking for that much here is what buys it: the area below
        // is padded to `align` and the frame is rounded to land the stack pointer back on it.
        if layout.grows {
            align = align.max(conv.stack_align);
        }

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

        // Above everything the function can reach through a local, which is the whole point of it.
        // A write that runs off the end of an array in this frame passes the canary before it
        // reaches the saved registers and the return address, so the check at the end of the
        // function sees a word that changed rather than a return that has already been taken.
        let mut canary = None;
        if layout.protect {
            top = top.next_multiple_of(word);
            canary = Some(offset(top));
            top += word;
        }

        // A call reads its stack arguments from the stack pointer upward, so the outgoing area is
        // at the bottom of the frame and its size is what shifts everything else.
        let outgoing = if layout.leaf { 0 } else { layout.outgoing.max(conv.shadow) };
        // Everything above it was placed as though it were not there, so moving it up by the size
        // of the area is what would break its alignment. The area is padded to the widest
        // alignment anything above it asked for, which costs at most that many bytes once and
        // costs nothing at all in the usual frame, where the area is a multiple of it already.
        // What the padding must not do is move the area itself: the callee reads its arguments
        // from the stack pointer, so the bottom of the area is the stack pointer whatever is
        // above it.
        let shifted = outgoing.next_multiple_of(align);
        let body = (top + shifted).next_multiple_of(word);

        let realign = (align > conv.stack_align).then_some(align);
        // Refused by [`crate::pipeline`] before anything gets here, because the two of them together
        // want one register twice. See `Growing` above.
        assert!(
            !(layout.grows && realign.is_some()),
            "a frame that grows and forces its alignment needs a second base register"
        );
        // Two frames keep one whatever the flags asked for, and each of them for its own version of
        // the same reason: the prologue is about to leave the stack pointer somewhere no constant
        // reaches the rest of the frame from, and the frame pointer is the register that still
        // does. Forcing an alignment is one of the two and growing while the function runs is the
        // other.
        let frame_pointer = layout.frame_pointer || realign.is_some() || layout.grows;

        // Where the stack pointer sits once the prologue has finished pushing: one return address
        // short of aligned when the function starts, and one word further off for every push. The
        // frame pointer is a push like any other here, which is why this is asked after the two
        // frames that keep one without being asked to have said so.
        let pushed = u32::from(frame_pointer) + u32::try_from(saved_int.len()).expect("a frame");
        let entry = wrap(conv.stack_align, conv.return_address);
        let after = (entry + wrap(conv.stack_align, word * pushed)) % conv.stack_align;

        // A frame that grows cannot be one of the free ones. The red zone is the bytes below the
        // stack pointer, and the first thing a variable length array does is move the stack pointer
        // down over them, so what was in the zone would be handed out twice.
        let free = layout.leaf
            && layout.red_zone
            && realign.is_none()
            && !layout.grows
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
        //
        // A frame that grows is counted from the frame pointer instead, which is the same areas in
        // the same order with one more constant taken off: the prologue pushed the registers and
        // then took the frame, so the body's stack pointer is that far below where the frame
        // pointer was set. That distance is what a variable length array destroys and the frame
        // pointer is what is left, which is why a growing frame keeps one.
        let mut shift = if free { -offset(body) } else { offset(shifted) };
        if layout.grows {
            shift -= offset(size) + offset(word) * i32::try_from(saved_int.len()).expect("a frame");
        }
        for at in slots
            .iter_mut()
            .chain(locals.iter_mut())
            .chain(canary.iter_mut())
            .chain(saved_sse.iter_mut().map(|save| &mut save.at))
        {
            *at += shift;
        }

        Self {
            saved_int,
            saved_sse,
            slots,
            locals,
            canary,
            outgoing,
            below: shifted,
            size,
            realign,
            incoming: if realign.is_some() || layout.grows {
                // The prologue saves the frame pointer before it does anything else and points it
                // at where it saved it, so the caller's stack is one word for that and one return
                // address above it, whatever the prologue did to the stack pointer afterwards.
                Incoming::from_frame(offset(word + conv.return_address))
            } else {
                Incoming::from_stack(offset(size + word * pushed + conv.return_address))
            },
            frame_pointer,
            grows: layout.grows,
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

    /// The vector registers the prologue stores into the frame, and where each of them goes.
    #[must_use]
    pub fn saved_sse(&self) -> &[Save] {
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

    /// Where the stack protector's canary is, from the stack pointer in the body of the function,
    /// or `None` in a frame that has none.
    #[must_use]
    pub fn canary(&self) -> Option<i32> {
        self.canary
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

    /// How many bytes at the bottom of the frame nothing else may be placed in, which is that area
    /// padded to the alignment everything above it asked for.
    ///
    /// What a variable length array has to step over. It takes its bytes off the stack pointer,
    /// which leaves them at the bottom of the frame where the next call is going to write its
    /// arguments, so the address it hands out is this far above the stack pointer rather than the
    /// stack pointer itself.
    #[must_use]
    pub fn below(&self) -> u32 {
        self.below
    }

    /// Whether the function moves the stack pointer while it runs.
    ///
    /// Every offset in the body of such a frame is from the frame pointer rather than from the
    /// stack pointer, because a variable length array leaves the stack pointer somewhere no
    /// constant reaches the rest of the frame from. See `Growing` in the module documentation.
    #[must_use]
    pub fn grows(&self) -> bool {
        self.grows
    }

    /// What the prologue has to force the stack pointer to be a multiple of, when a local wants
    /// more alignment than a call leaves it with.
    #[must_use]
    pub fn realign(&self) -> Option<u32> {
        self.realign
    }

    /// Where the first argument the caller passed on the stack is, and which register reaches it.
    ///
    /// The only offset here that is not always from the stack pointer. A realigned frame counts
    /// from the frame pointer instead, because forcing the alignment threw away however far the
    /// caller's stack pointer was from where the prologue wanted it, and the frame pointer is what
    /// reaches the caller's stack afterwards.
    #[must_use]
    pub fn incoming(&self) -> Incoming {
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
        let allocation = rucc_regalloc::run(&mut func, &env(conv, count), "test");
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
        assert_eq!(frame.incoming(), Incoming::from_stack(8));
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
        assert_eq!(frame.incoming(), Incoming::from_stack(8));
    }

    #[test]
    fn a_leaf_function_told_it_has_no_red_zone_takes_the_bytes_instead() {
        let (func, allocation) = pressure(&SYSV, 4, 2);
        let base = Layout::new(&SYSV, REGS);
        let frame = Frame::of(&func, &allocation, &Layout { red_zone: false, ..base });

        assert_eq!(frame.size(), 16);
        assert_eq!((frame.slot(0), frame.slot(1)), (Some(0), Some(8)));
        assert_eq!(frame.incoming(), Incoming::from_stack(24));
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
        assert_eq!(frame.incoming(), Incoming::from_stack(32));
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
        assert_eq!(frame.incoming(), Incoming::from_stack(32));
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
        let allocation = rucc_regalloc::run(&mut func, &env(&SYSV, 4), "test");
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
        // prologue wanted it, so a frame pointer is needed and the caller's stack is reached
        // through it instead: one word for the saved frame pointer and one for the return address.
        assert!(frame.frame_pointer());
        assert_eq!(frame.incoming(), Incoming::from_frame(16));
    }

    #[test]
    fn the_canary_is_above_every_byte_a_local_or_a_spill_reaches() {
        let (func, allocation) = pressure(&SYSV, 4, 2);
        let locals = [Local { size: 16, align: 16 }, Local { size: 8, align: 8 }];
        let base = Layout::new(&SYSV, REGS);
        let there = Layout { leaf: false, locals: &locals, protect: true, ..base };
        let frame = Frame::of(&func, &allocation, &there);

        // Two spill slots at the bottom, then the two locals, then the canary above all four. That
        // order is the whole mechanism: a write that runs off the end of either local passes the
        // canary before it reaches the saved registers and the return address.
        let canary = frame.canary().expect("a protected frame has a slot");
        for below in [frame.slot(0), frame.slot(1), frame.local(0), frame.local(1)] {
            assert!(below.expect("a slot that was asked for") < canary);
        }
        assert_eq!(canary, 40);
        // Forty eight bytes of areas, and then the eight that put the stack pointer back where a
        // call wants it, because the arm the check fails on makes one.
        assert_eq!(frame.size(), 56);
        assert_eq!((frame.size() + SYSV.return_address) % SYSV.stack_align, 0);
    }

    #[test]
    fn a_frame_with_no_protector_has_no_slot_for_a_canary() {
        let (func, allocation) = pressure(&SYSV, 2, 4);
        let frame = Frame::of(&func, &allocation, &Layout::new(&SYSV, REGS));

        assert_eq!(frame.canary(), None);
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

    /// Moving everything up by the size of the outgoing area is what would break its alignment,
    /// so the area is padded to the widest alignment anything above it wanted. The area itself
    /// still starts at the stack pointer, because that is the one thing about it that is not this
    /// frame's to choose.
    #[test]
    fn what_is_above_the_outgoing_area_keeps_the_alignment_it_asked_for() {
        let (func, allocation) = pressure(&SYSV, 2, 4);
        let locals = [Local { size: 16, align: 16 }];
        let base = Layout::new(&SYSV, REGS);
        let there = Layout { leaf: false, outgoing: 8, locals: &locals, ..base };
        let frame = Frame::of(&func, &allocation, &there);

        assert_eq!(frame.outgoing(), 8);
        assert_eq!(frame.local(0), Some(16));
        assert_eq!(frame.size(), 40);
        // A call leaves the stack pointer one return address short of aligned and nothing was
        // pushed on top of that, so the frame is what puts it back and the local lands aligned.
        assert_eq!((frame.size() + SYSV.return_address) % SYSV.stack_align, 0);
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
        assert_eq!(frame.incoming(), Incoming::from_stack(48));
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
