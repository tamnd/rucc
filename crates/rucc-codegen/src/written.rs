//! What a function writes of the registers a call may destroy, which is gcc's `-fipa-ra`.
//!
//! A call is taken to destroy every register the convention does not keep, because that is all a
//! caller can know of a function it has not seen. A function this file has already compiled is one
//! it has seen, and the registers its code writes are there to be read. gcc keeps that set for each
//! function it has finished and reads it at a call to one, so a value that lives across the call
//! can stay in a register the callee never touches rather than in one the caller has to save and
//! put back. The order the functions are compiled in puts a callee ahead of its callers where it
//! can, which is what makes the answer there when a caller asks. See `rucc_opt::expand`.
//!
//! The answer is read off the code as it is written, after every pass: each register an
//! instruction writes, and for each way out of the function that is not a return, what the place
//! it goes to writes. A call or a jump to a name with no answer of its own, or through a register,
//! could write anything, and then there is no answer for this function either. A call that never
//! comes back is the exception, since nothing it writes is seen by the caller.
//!
//! Whose answers a call may use is the driver's question, since it is about the link rather than
//! the code: a name another object may replace is not sure to reach this code at all.

use rucc_base::Interner;
use rucc_mir as mir;
use rucc_target::{PhysReg, RegClass, Speculation, Thunk};

use crate::elsewhere::Elsewhere;
use crate::pipeline::Machine;

/// The registers a call may destroy that `func` writes, with what each function it calls or jumps
/// to writes, or `None` when that cannot be known.
///
/// `speculation` is what the function was compiled with. A jump through a register written into
/// the text of a thunk by `-mindirect-branch=thunk-inline` is not an instruction this can tell
/// from any other text, so a function compiled that way has no answer.
#[must_use]
pub fn registers(
    func: &mir::Func,
    machine: &Machine,
    elsewhere: &Elsewhere,
    speculation: Speculation,
    names: &Interner,
) -> Option<Vec<(PhysReg, RegClass)>> {
    if speculation.indirect == Thunk::Inline {
        return None;
    }
    let conv = machine.conv;
    let insts = machine.insts;
    let returned = insts.thunks.map(|thunks| thunks.ret);
    let destroyed = |reg: PhysReg, class: RegClass| {
        (class == conv.int_class && !conv.preserves_int(reg))
            || (class == conv.sse_class && !conv.preserves_sse(reg))
    };
    let mut written: Vec<(PhysReg, RegClass)> = Vec::new();
    let mut add = |reg: PhysReg, class: RegClass| {
        if destroyed(reg, class) && !written.contains(&(reg, class)) {
            written.push((reg, class));
        }
    };
    for block in func.blocks() {
        let all: Vec<mir::Inst> = func.insts(block).collect();
        let ends = func[block].succs.is_empty();
        for (at, &inst) in all.iter().enumerate() {
            for operand in &func[func[inst].operands] {
                let reg = operand.reg.phys()?;
                if operand.role.is_def() {
                    add(reg, operand.class);
                }
            }
            let name = names.resolve(func[inst].opcode.name());
            let bare = name.strip_prefix(insts.prefix).unwrap_or(name);
            let symbol = func[inst].symbol;
            if machine.shapes.calls(name) {
                // One at the end of a block that goes nowhere, with nothing after it but a trap,
                // is a call that does not come back.
                let never = ends
                    && all[at + 1..].iter().all(|&after| {
                        let after = names.resolve(func[after].opcode.name());
                        let after = after.strip_prefix(insts.prefix).unwrap_or(after);
                        after == machine.selector.trap
                            || insts.thunks.is_some_and(|thunks| after == thunks.trap)
                    });
                if never {
                    continue;
                }
                for &(reg, class) in elsewhere.written(symbol?)? {
                    add(reg, class);
                }
            } else if Some(bare) == insts.away {
                let symbol = symbol?;
                if returned.is_some_and(|ret| names.resolve(symbol) == ret) {
                    continue;
                }
                for &(reg, class) in elsewhere.written(symbol)? {
                    add(reg, class);
                }
            } else if ends && leaves(bare, machine) {
                return None;
            }
        }
    }
    Some(written)
}

/// Whether an instruction of that name is a jump through a register or through memory, which at
/// the end of a block that goes nowhere is a jump out of the function.
fn leaves(bare: &str, machine: &Machine) -> bool {
    let insts = machine.insts;
    bare == machine.branch.indirect
        || insts.through.is_some_and(|through| bare == through.jump.0 || bare == through.jump.1)
        || insts.thunks.is_some_and(|thunks| {
            bare == thunks.jump || bare == thunks.jump_padded || bare == thunks.jump_through
        })
}
