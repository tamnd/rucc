//! Turning the checks that survived the optimizer into something the back end can generate.
//!
//! Design: `spec/safe-memory/06-instrumentation.md` sections 6.3.1 and 6.5.
//!
//! [`crate::insert`] puts checks in before the optimizer runs, which is the whole argument of this
//! crate. This is the other end of that: after the optimizer has discharged what it could prove,
//! every check still standing becomes a call to `rucc-safe-rt`, and each call carries the index of
//! a row in a table this pass builds and puts in the object.
//!
//! # Why the checks are calls and not compares
//!
//! Section 6.3.1 wants a compare and a branch in the checked function with only the trap out of
//! line, and that is not what this emits. The reason is in `rucc-safe-rt`'s `check` module and is
//! the same one: the inline form needs the four word capability of document 05 section 5.2.1 live
//! in registers at the check, and it needs the aux plane to recover one for a pointer that came out
//! of memory. The capability representation is milestone S2 and the aux plane is S5. Handing the
//! runtime an address is what can be written today.
//!
//! It is slow, and S1's exit criterion asks for the overhead to be measured rather than for it to
//! be small. S4 is the milestone that makes it small, and it needs a number to improve on.
//!
//! # Why it runs after the optimizer
//!
//! Because the id in a call is an index into a table, and a table with rows for checks that were
//! deleted is a table that is mostly padding. Running here means a row exists for exactly the
//! checks a program will actually run, which is also what makes the row count a number worth
//! reporting. The cost is that `--emit=ir` shows `check_bounds` rather than the call, which is the
//! right way round: the IR a person reads should say what the compiler decided, not how it spelled
//! it.
//!
//! # What the table is
//!
//! One sixteen byte row per surviving check, in a section called `.rucc_safety_desc`, laid out the
//! way `rucc_safe_rt::fail::Descriptor` reads it. Nothing links against it by name: the reporter
//! milestone S2 writes finds it by section, which is how a runtime that was built separately from
//! the program reaches it at all.
//!
//! The id is an index into *this object's* table. Link two instrumented objects together and the
//! sections concatenate while both sets of indices still start at zero, so a reporter that read the
//! whole section by index would name the wrong check. Nothing reads the table yet, so nothing is
//! wrong today, and the reporter is what has to settle it: either every object contributes its base
//! or the id becomes a relocated address rather than an index. It is written down here so that
//! whoever writes the reporter meets it as a decision rather than as a bug.

use rucc_base::Interner;
use rucc_ir::{
    CallInfo, Datum, Extra, Flags, Func, Global, Imm, Inst, InstData, Linkage, Module, Opcode,
    Signature, Type, Value,
};

/// How wide one row of the table is, which `rucc_safe_rt::fail::Descriptor` fixes.
pub const WIDTH: u64 = 16;

/// The section the rows go in, which is how the runtime finds them.
pub const SECTION: &str = ".rucc_safety_desc";

/// The name the table is given, which nothing outside the object uses.
const TABLE: &str = "__rucc_safety_desc";

/// Judgement J1 of document 04 section 4.4, which is what an access check decides.
const ACCESS: u8 = 1;

/// Judgement J2, which is what a derivation check decides.
const DERIVE: u8 = 2;

/// The type a descriptor id is passed as, which is `u32` on the runtime's side.
const ID: Type = Type::int(32);

/// One row of the table, as much of it as this pass knows.
///
/// The `pc` field of the runtime's descriptor is not here. Filling it means a relocation against
/// the enclosing function plus the offset of the call, which the IR cannot express and which
/// nothing needs until there is a reporter to read it, so those eight bytes are written as zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Descriptor {
    /// Which judgement the check decides.
    pub judgement: u8,
    /// Which row of document 03's tables the failure is, which nothing decides yet.
    pub class: u8,
    /// How many bytes the access covers, saturating, and zero where the check is not about an
    /// access of a known width.
    pub size: u16,
}

/// Turns every check in a module into a call, and gives the module the table those calls index.
///
/// The number of rows, which is the number of checks that survived the optimizer. That is the
/// numerator of everything document 13 measures and it is not recoverable afterwards, since by
/// this point a discharged check is simply not there.
///
/// Whether this runs at all is `-fsafety=`, and the driver decides it, for the reason
/// [`crate::run`] gives.
pub fn lower(module: &mut Module, names: &mut Interner) -> usize {
    // `size_t`, taken from the module rather than written as sixty four, so that the argument is
    // the one the runtime's own declaration of the entry point has.
    let word = Type::int(module.datalayout.pointer_bits);
    let mut table: Vec<Descriptor> = Vec::new();
    for id in module.funcs() {
        if module[id].is_declaration() {
            continue;
        }
        calls(&mut module[id], names, word, &mut table);
    }
    if !table.is_empty() {
        emit(module, names, &table);
    }
    table.len()
}

/// Rewrites every check in one function, and takes the capabilities out afterwards.
fn calls(func: &mut Func, names: &mut Interner, word: Type, table: &mut Vec<Descriptor>) {
    let insts: Vec<Inst> =
        func.blocks().flat_map(|block| func.insts(block).collect::<Vec<_>>()).collect();
    for &inst in &insts {
        match func[inst].opcode {
            Opcode::CheckBounds => bounds(func, names, word, table, inst),
            Opcode::CheckLive => live(func, names, table, inst),
            Opcode::CheckDeriv => deriv(func, names, table, inst),
            _ => {}
        }
    }
    // Every `cap_of` in the function was put there to feed a check, and no check reads one any
    // more. They are removed rather than left for the optimizer because the optimizer has already
    // run, and a `cap` is a type the back end has never been taught.
    for &inst in &insts {
        if func[inst].opcode == Opcode::CapOf {
            func.remove_inst(inst);
        }
    }
}

/// `check_bounds` becomes `__rucc_check_bounds(pointer, size, id)`.
fn bounds(
    func: &mut Func,
    names: &mut Interner,
    word: Type,
    table: &mut Vec<Descriptor>,
    inst: Inst,
) {
    let [_capability, pointer] = func[func[inst].args] else { return };
    let Extra::Mem(mem) = func[inst].extra else { return };
    let size = func[mem].size;

    let row = Descriptor {
        judgement: ACCESS,
        class: 0,
        // Saturating, so that a report about a structure copy larger than a row can hold says
        // sixty five thousand rather than whatever the low sixteen bits happened to be.
        size: u16::try_from(size).unwrap_or(u16::MAX),
    };
    let id = record(table, row);
    let bytes = konst(func, inst, Imm::int(i128::from(size), word), word);
    let id = konst(func, inst, Imm::int(i128::from(id), ID), ID);
    call(func, names, inst, "__rucc_check_bounds", &[Type::PTR, word, ID], &[pointer, bytes, id]);
}

/// `check_live` becomes `__rucc_check_live(pointer, id)`.
fn live(func: &mut Func, names: &mut Interner, table: &mut Vec<Descriptor>, inst: Inst) {
    let [_capability, pointer] = func[func[inst].args] else { return };
    // No size. The check carries no payload, because whether anybody owns an address is a question
    // about the address rather than about how many bytes are read through it.
    let id = record(table, Descriptor { judgement: ACCESS, class: 0, size: 0 });
    let id = konst(func, inst, Imm::int(i128::from(id), ID), ID);
    call(func, names, inst, "__rucc_check_live", &[Type::PTR, ID], &[pointer, id]);
}

/// `check_deriv` becomes `__rucc_check_deriv(base, derived, id)`.
fn deriv(func: &mut Func, names: &mut Interner, table: &mut Vec<Descriptor>, inst: Inst) {
    let [_capability, base, derived] = func[func[inst].args] else { return };
    let id = record(table, Descriptor { judgement: DERIVE, class: 0, size: 0 });
    let id = konst(func, inst, Imm::int(i128::from(id), ID), ID);
    call(
        func,
        names,
        inst,
        "__rucc_check_deriv",
        &[Type::PTR, Type::PTR, ID],
        &[base, derived, id],
    );
}

/// Adds a row to the table and gives back the index the call passes.
fn record(table: &mut Vec<Descriptor>, row: Descriptor) -> u32 {
    let id = u32::try_from(table.len()).unwrap_or(u32::MAX);
    table.push(row);
    id
}

/// Puts an integer constant in front of `inst` and gives back what it produced.
fn konst(func: &mut Func, inst: Inst, imm: Imm, ty: Type) -> Value {
    let span = func.span(inst);
    let extra = Extra::Imm(func.add_imm(imm));
    let made = func.create_inst(InstData { extra, ..InstData::new(Opcode::IConst) }, &[ty], span);
    func.insert_before(made, inst);
    func[made].results().next().expect("a constant created with one result has one")
}

/// Turns `inst` into a call of `routine` with those arguments, in place.
///
/// In place rather than as a new instruction beside it, because the check is already where it has
/// to be: in front of the access for the two access checks and behind the arithmetic for the
/// derivation one. Moving it would be a chance to get that wrong.
fn call(
    func: &mut Func,
    names: &mut Interner,
    inst: Inst,
    routine: &str,
    params: &[Type],
    args: &[Value],
) {
    let sig = func.add_signature(Signature::new().with_params(params));
    let callee = names.intern(routine);
    // Nothing is passed past the last named parameter, so there is nothing for the ABI to say
    // about the arguments the signature does not name.
    let varargs = func.push_abis(&[]);
    let info = func.add_call(CallInfo { callee: Some(callee), signature: sig, varargs });
    let args = func.push_values(args);
    let data = &mut func[inst];
    data.opcode = Opcode::Call;
    data.args = args;
    data.extra = Extra::Call(info);
    data.flags = data.flags.intersection(Flags::legal_on(Opcode::Call));
}

/// Adds the table to the module as a variable in its own section.
///
/// Internal, so the linker never has to resolve the name and two objects in a link do not collide
/// over it. Constant, because nothing writes a descriptor after the compiler has. Eight byte
/// aligned, because the runtime reads the rows as a `#[repr(C)]` structure with a `u64` in it.
fn emit(module: &mut Module, names: &mut Interner, table: &[Descriptor]) {
    let byte = Type::int(8);
    let half = Type::int(16);
    let mut image = Vec::with_capacity(table.len() * 4);
    for row in table {
        let judgement = module.add_imm(Imm::int(i128::from(row.judgement), byte));
        let class = module.add_imm(Imm::int(i128::from(row.class), byte));
        let size = module.add_imm(Imm::int(i128::from(row.size), half));
        image.push(Datum::Scalar { ty: byte, value: judgement });
        image.push(Datum::Scalar { ty: byte, value: class });
        image.push(Datum::Scalar { ty: half, value: size });
        // Four bytes the C layout puts in front of the `u64`, and then the eight of the program
        // counter, which nothing fills in yet. Both are zero and both are written out rather than
        // left off, because the row after this one has to start sixteen bytes along.
        image.push(Datum::Zero(4));
        image.push(Datum::Zero(8));
    }
    let init = module.push_data(&image);
    let size = table.len() as u64 * WIDTH;
    let mut global = Global::new(names.intern(TABLE), size, 8);
    global.linkage = Linkage::Internal;
    global.constant = true;
    global.section = Some(names.intern(SECTION));
    global.init = Some(init);
    module.add_global(global);
}

#[cfg(test)]
mod tests {
    use rucc_ir::{Builder, MemInfo, MemOrder, Restrict, print_func, verify_func};
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    use super::*;
    use crate::insert;

    fn target() -> TargetInfo {
        TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu))
    }

    /// A module holding one function that loads through its parameter, with checks already in.
    fn checked(names: &mut Interner) -> Module {
        let i32_ = Type::int(32);
        let mut func = Func::new(
            names.intern("read"),
            Signature::new().with_params(&[Type::PTR]).with_returns(&[i32_]),
        );
        let entry = func.create_block();
        let p = func.append_param(entry, Type::PTR);

        let info = MemInfo {
            size: 4,
            align: 4,
            order: MemOrder::NotAtomic,
            tbaa: None,
            restrict: Restrict::NONE,
        };
        let mut b = Builder::new(&mut func, entry);
        let args = b.func().push_values(&[p]);
        let extra = Extra::Mem(b.func().add_mem(info));
        let loaded = b.value(InstData { args, extra, ..InstData::new(Opcode::Load) }, i32_);
        b.ret(&[loaded]);

        insert(&mut func);
        let mut module = Module::new(names.intern("read.c"), &target());
        module.add_func(func);
        module
    }

    #[test]
    fn every_check_becomes_a_call_carrying_the_row_it_is_described_by() {
        let mut names = Interner::new();
        let mut module = checked(&mut names);
        assert_eq!(lower(&mut module, &mut names), 2);

        let id = module.funcs().next().expect("the module has one function");
        assert_eq!(
            print_func(&module, &module[id], &names),
            "func @read(ptr) -> i32, linkage(external) {\n\
             block0(%0: ptr):\n    \
             %1 = iconst.i64 4\n    \
             %2 = iconst.i32 0\n    \
             call @__rucc_check_bounds(%0, %1, %2) : (ptr, i64, i32)\n    \
             %3 = iconst.i32 1\n    \
             call @__rucc_check_live(%0, %3) : (ptr, i32)\n    \
             %4 = load.i32 %0, size 4, align 4\n    \
             return %4\n\
             }\n"
        );
    }

    #[test]
    fn the_capabilities_the_checks_were_reading_are_taken_out() {
        // A `cap` is a type nothing in the back end has been taught, so one left behind is a
        // compilation that fails rather than a value nobody reads.
        let mut names = Interner::new();
        let mut module = checked(&mut names);
        lower(&mut module, &mut names);

        let id = module.funcs().next().expect("the module has one function");
        let func = &module[id];
        let left: Vec<Opcode> = func
            .blocks()
            .flat_map(|block| func.insts(block).collect::<Vec<_>>())
            .map(|inst| func[inst].opcode)
            .collect();
        assert!(!left.contains(&Opcode::CapOf), "{left:?}");
    }

    #[test]
    fn what_it_produces_is_a_module_the_verifier_believes() {
        let mut names = Interner::new();
        let mut module = checked(&mut names);
        lower(&mut module, &mut names);

        let id = module.funcs().next().expect("the module has one function");
        if let Err(errors) = verify_func(&module, &module[id], &names) {
            panic!("that was expected to be believed: {errors:#?}");
        }
    }

    #[test]
    fn the_table_is_one_row_per_check_in_the_section_the_runtime_reads() {
        let mut names = Interner::new();
        let mut module = checked(&mut names);
        let rows = lower(&mut module, &mut names);

        let id = module.globals().next().expect("the table was added");
        let table = &module[id];
        assert_eq!(names.resolve(table.name), TABLE);
        assert_eq!(names.resolve(table.section.expect("the table names its section")), SECTION);
        assert_eq!(table.linkage, Linkage::Internal);
        assert!(table.constant);
        assert_eq!(table.align, 8);
        assert_eq!(table.size, rows as u64 * WIDTH);

        // The image has to add up to the size, or the row a descriptor id names is not the row
        // the compiler meant.
        let init = table.init.expect("the table is a definition");
        let written: u64 = module[init].iter().map(|datum| datum.size(&module)).sum();
        assert_eq!(written, table.size);
    }

    #[test]
    fn the_judgement_a_row_names_is_the_one_the_check_decides() {
        // A report that said J1 where the program derived a pointer would send somebody looking
        // at the wrong line, so the two rows a derivation produces are checked by hand.
        let mut names = Interner::new();
        let mut func = Func::new(
            names.intern("walk"),
            Signature::new().with_params(&[Type::PTR, Type::int(64)]).with_returns(&[Type::PTR]),
        );
        let entry = func.create_block();
        let p = func.append_param(entry, Type::PTR);
        let n = func.append_param(entry, Type::int(64));
        let mut b = Builder::new(&mut func, entry);
        let args = b.func().push_values(&[p, n]);
        let moved = b.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        b.ret(&[moved]);
        insert(&mut func);

        let mut table = Vec::new();
        calls(&mut func, &mut names, Type::int(64), &mut table);
        assert_eq!(table, [Descriptor { judgement: DERIVE, class: 0, size: 0 }]);
    }

    #[test]
    fn a_module_with_nothing_to_check_gets_no_table_at_all() {
        // An object with an empty section in it is an object that says the compiler had something
        // to say and did not say it.
        let mut names = Interner::new();
        let mut module = Module::new(names.intern("empty.c"), &target());
        assert_eq!(lower(&mut module, &mut names), 0);
        assert_eq!(module.globals().count(), 0);
    }
}
