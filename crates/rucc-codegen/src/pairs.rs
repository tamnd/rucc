//! Two loads or two stores of words next to each other put together into one `ldp` or `stp` on
//! AArch64, as gcc writes them.
//!
//! The machine reads or writes two registers at two neighbouring words of memory in one
//! instruction, which is one slot in the load or store pipeline where two loads take two. A
//! structure of two longs copied, two fields read for a sum, the four corners of a small matrix:
//! every one of those is two accesses at the same base with offsets one word apart.
//!
//! # Why after allocation
//!
//! Which two accesses are next to each other is only settled once the schedule has had the
//! block, and whether two loads can share an instruction depends on the registers they were
//! given: `ldp` cannot write the same register twice, and the first load cannot have written the
//! base the second reads. Both are questions about physical registers.
//!
//! # What may stand between the two
//!
//! A pair of loads goes where the first load was, so the second moves up, and a pair of stores
//! goes where the second store was, so the first moves down. That way round the registers a store
//! reads are all written by the time it happens, which they are often not where the first store
//! was: the value of the second is commonly written by the instruction right in front of it.
//!
//! Nothing in between may write memory, since there is no alias information here and a store in
//! between could be to either word. Between two loads another plain load of the table may stand,
//! as in `a[0] * b[0] + a[1] * b[1]`, where the two loads of `a` and the two of `b` are written
//! in turn; between two stores nothing may touch memory at all, since the first store moves down
//! past it and the load could be of the word it writes. Nothing in between may write the base. For loads,
//! nothing in between may read or write the register the second load writes, since it is now
//! written earlier. For stores, nothing in between may write the register the first store reads,
//! since it is now read later. A call or
//! an instruction the target does not name ends the search, and so does a volatile access, which
//! the program asked to see as the access it wrote.
//!
//! An access off the stack pointer is left alone. The frame's own saves and restores are written
//! as pairs already, and the Windows unwind codes read the stores off the stack pointer in a
//! prologue as the saves they describe.

use rucc_base::Interner;
use rucc_base::hash::Map;
use rucc_mir::{Flags, Func, Inst, Mem, Opcode, Operand};
use rucc_target::{MachineInsts, PhysReg};

/// The single loads and stores on AArch64, the pair each one goes into, and the size of the word.
/// The floating point and vector ones pair the same way, into `ldp` and `stp` of `s`, `d` and `q`
/// registers, which is how gcc copies a structure of two doubles.
pub const A64_PAIRS: [(&str, &str, i32); 10] = [
    ("a64.ldr_32", "a64.ldp_32", 4),
    ("a64.ldr_64", "a64.ldp_64", 8),
    ("a64.str_32", "a64.stp_32", 4),
    ("a64.str_64", "a64.stp_64", 8),
    ("a64.ldr_f32", "a64.ldp_f32", 4),
    ("a64.ldr_f64", "a64.ldp_f64", 8),
    ("a64.ldr_f128", "a64.ldp_f128", 16),
    ("a64.str_f32", "a64.stp_f32", 4),
    ("a64.str_f64", "a64.stp_f64", 8),
    ("a64.str_f128", "a64.stp_f128", 16),
];

/// How many instructions past the first access the second is looked for in.
const WINDOW: usize = 4;

/// One of the accesses in the table.
#[derive(Clone, Copy)]
struct Access {
    /// The register it loads into or stores out of.
    value: Operand,
    /// The register the address is in.
    base: Operand,
    /// What is added to it.
    disp: i32,
    /// The pair it goes into, and the size of its word.
    pair: (Opcode, i32),
}

/// Puts together every two accesses that can be one, and gives back how many pairs it wrote.
///
/// `pairs` is the table, which is [`A64_PAIRS`] on AArch64 and empty anywhere else.
pub fn pairs(
    func: &mut Func,
    machine: &MachineInsts,
    pairs: &[(&str, &str, i32)],
    stack: PhysReg,
    names: &mut Interner,
) -> usize {
    if pairs.is_empty() {
        return 0;
    }
    let table: Map<Opcode, (Opcode, i32)> = pairs
        .iter()
        .map(|&(one, two, size)| {
            (Opcode::new(names.intern(one)), (Opcode::new(names.intern(two)), size))
        })
        .collect();
    let mut stops: Map<Opcode, bool> = Map::default();
    let mut made = 0;
    for block in func.blocks().collect::<Vec<_>>() {
        let insts: Vec<Inst> = func.insts(block).collect();
        let mut taken = vec![false; insts.len()];
        // The second load of a pair, which is gone from its place, since the pair is where the
        // first load was. A search from a load in between goes on past it.
        let mut moved = vec![false; insts.len()];
        for at in 0..insts.len() {
            if taken[at] {
                continue;
            }
            let Some(first) = access(func, insts[at], &table, stack) else { continue };
            let load = first.value.role.is_def();
            let mut between: Vec<Inst> = Vec::new();
            for next in at + 1..insts.len().min(at + 1 + WINDOW) {
                if taken[next] {
                    if load && moved[next] {
                        continue;
                    }
                    break;
                }
                let inst = insts[next];
                if let Some(second) = access(func, inst, &table, stack) {
                    if partners(func, &first, &second, &between) {
                        let pair = pair(func, insts[at], &first, &second);
                        func.insert_before(if load { insts[at] } else { inst }, pair);
                        func.remove_inst(insts[at]);
                        func.remove_inst(inst);
                        taken[at] = true;
                        taken[next] = true;
                        moved[next] = load;
                        made += 1;
                        break;
                    }
                    if !(load && second.value.role.is_def()) {
                        break;
                    }
                    between.push(inst);
                    continue;
                }
                let opcode = func[inst].opcode;
                let stop = *stops.entry(opcode).or_insert_with(|| {
                    let name = names.resolve(opcode.name());
                    machine.calls(name) || !machine.has(name) || machine.touches_mem(name)
                });
                if stop {
                    break;
                }
                between.push(inst);
            }
        }
    }
    made
}

/// The access that instruction is, when it is one of the table's at a base register and an
/// offset and nothing else.
fn access(
    func: &Func,
    inst: Inst,
    table: &Map<Opcode, (Opcode, i32)>,
    stack: PhysReg,
) -> Option<Access> {
    let data = func[inst];
    let &pair = table.get(&data.opcode)?;
    if data.imm.is_some() || data.symbol.is_some() || data.flags.contains(Flags::VOLATILE) {
        return None;
    }
    let amode = func[data.mem?];
    let plain = amode.index.is_none()
        && amode.symbol.is_none()
        && amode.block.is_none()
        && amode.table.is_none()
        && amode.segment.is_none()
        && amode.widen.is_none();
    if !plain {
        return None;
    }
    let [value, base] = func[data.operands] else { return None };
    if amode.base != Some(1) || base.role.is_def() || base.reg.phys()? == stack {
        return None;
    }
    value.reg.phys()?;
    Some(Access { value, base, disp: amode.disp, pair })
}

/// Whether the second access can be put together with the first, across what stands between.
fn partners(func: &Func, first: &Access, second: &Access, between: &[Inst]) -> bool {
    let (pair, size) = first.pair;
    if second.pair.0 != pair || !same(&second.base, &first.base) {
        return false;
    }
    if first.disp.abs_diff(second.disp) != size.unsigned_abs() {
        return false;
    }
    let low = first.disp.min(second.disp);
    if low % size != 0 || low < -64 * size || low > 63 * size {
        return false;
    }
    let load = first.value.role.is_def();
    if load && (same(&first.value, &first.base) || same(&first.value, &second.value)) {
        return false;
    }
    between.iter().all(|&inst| {
        func[func[inst].operands].iter().all(|operand| {
            let writes = operand.role.is_def();
            if writes && same(operand, &first.base) {
                return false;
            }
            if load {
                !same(operand, &second.value)
            } else {
                !(writes && same(operand, &first.value))
            }
        })
    })
}

/// Whether two operands are the same register, which takes the file as well as the number, since
/// `x1` and `v1` are both register one.
fn same(a: &Operand, b: &Operand) -> bool {
    a.reg == b.reg && a.class == b.class
}

/// The one instruction the two accesses become, loose, with the first one's flags and place in
/// the source.
fn pair(func: &mut Func, at: Inst, first: &Access, second: &Access) -> Inst {
    let (low, high) = if first.disp < second.disp { (first, second) } else { (second, first) };
    let flags = func[at].flags;
    let span = func.span(at);
    let mem = Mem { disp: low.disp, ..Mem::at(first.base) };
    func.build_loose(first.pair.0)
        .operand(low.value)
        .operand(high.value)
        .mem(mem)
        .flags(flags)
        .at(span)
        .finish()
}
