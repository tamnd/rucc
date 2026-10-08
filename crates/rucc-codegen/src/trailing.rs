//! A trap after a call that ends a function, which x86-64 Windows needs.
//!
//! The unwinder there finds the record of a frame by looking up the address its call returns to
//! in `.pdata`, which says which bytes each function covers. A call to a function that does not
//! come back, such as `longjmp` or `abort`, is often the last instruction of the function it is in,
//! because the lowering writes nothing for the `unreachable` behind it. The address the call
//! returns to is then the first byte past the end, which belongs to the next function or to none,
//! and the unwinder reads the frame with the wrong record or as a leaf. `longjmp` on Windows is an
//! unwind, so a `longjmp` through such a frame stops the program. That is how `jmpbuf`, `longjmp`
//! and `realign` in `tests/exec/windows` failed at `-O2`, where the block with the call is laid
//! out last, without printing a line.
//!
//! clang writes an `int3` after such a call and gcc a `nop`. This writes the trap the lowering
//! writes for `__builtin_trap`, so the address after the call is inside the function, and nothing
//! ever runs it.

use rucc_base::Interner;
use rucc_mir as mir;
use rucc_target::MachineInsts;

use crate::select::Selector;

/// Puts the trap after the last instruction of the function when that instruction is a call.
///
/// Once the blocks are in their final order, since which block is last is the layout's answer.
pub fn trap(
    func: &mut mir::Func,
    selector: &Selector,
    shapes: &MachineInsts,
    names: &mut Interner,
) {
    let Some(last) = func.blocks().last() else { return };
    let Some(inst) = func.terminator(last) else { return };
    if !shapes.calls(names.resolve(func[inst].opcode.name())) {
        return;
    }
    let trap = mir::Opcode::new(names.join(selector.prefix(), selector.trap));
    let span = func.span(inst);
    let stop = func.build_loose(trap).at(span).finish();
    func.insert_after(inst, stop);
}
