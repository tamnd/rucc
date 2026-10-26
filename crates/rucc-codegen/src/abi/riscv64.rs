//! The instructions the calling convention is written with on RISC-V.
//!
//! The same questions [`super::aarch64`] answers, with the names `rucc_target::riscv64` describes.
//! LP64D keeps a 32 bit value sign extended in its 64 bit register, which is what the rule file
//! promises about one too, so an `int` arrives and goes back as the 32 bit pseudo and nothing has
//! to be done to it on either side. A byte and a short travel the same way, extended to 32 bits by
//! their own sign. The rules promise nothing about their high bits, so a value that arrives like
//! that is read the same as one that does not. `_Float16`, `long double` and `__int128` have no
//! rules on this machine yet, so they have no names here either, and a function that passes one is
//! refused at the argument rather than halfway through.

use rucc_ir::{Abi, Type};

use super::{Insts, place};

/// The RISC-V ones.
pub static INSTS: Insts = Insts {
    arg: head_of,
    load: load_of,
    store: store_of,
    word: store_of,
    ret: ret_of,
    call: "rv.call",
    call_reg: "rv.call_reg",
    lea: "rv.lea_64",
    small: "rv.li_32",
    extend: extend_of,
};

/// What the instruction that extends a narrow integer to 32 bits is called, for a value the
/// signature says travels extended.
///
/// LP64D says every integer narrower than a word is extended by its own sign to 32 bits and then
/// to the whole register, by the caller for an argument and by the callee for a return value. The
/// second half is what keeping a 32 bit value sign extended already does.
fn extend_of(ty: Type, abi: Abi) -> Option<&'static str> {
    match (abi, ty.bits()) {
        (Abi::Zext, 1) => Some("rv.bit_to_32"),
        (Abi::Sext, 8) => Some("rv.sextb_32"),
        (Abi::Sext, 16) => Some("rv.sexth_32"),
        (Abi::Zext, 8) => Some("rv.zextb_32"),
        (Abi::Zext, 16) => Some("rv.zexth_32"),
        _ => None,
    }
}

/// Whether the type is one this machine has no rules for yet.
fn untaught(ty: Type) -> bool {
    crate::term::is_quad(ty) || crate::term::is_half(ty)
}

/// What the pseudo for an argument of that type is called.
fn head_of(ty: Type) -> Option<&'static str> {
    if untaught(ty) {
        return None;
    }
    if let Some(at) = crate::term::float_slot(ty) {
        return Some(["rv.arg_val_f32", "rv.arg_val_f64"][at]);
    }
    let names = ["rv.arg_val_32", "rv.arg_val_32", "rv.arg_val_32", "rv.arg_val_64"];
    Some(names[place(ty, 64)?])
}

/// What the instruction that reads an argument of that type out of memory is called.
///
/// A narrow one is read at its own width, for the reason [`super::load_of`] gives. A word is read
/// with `lw`, which sign extends it, so it is kept the way the rules want it.
fn load_of(ty: Type) -> Option<&'static str> {
    if untaught(ty) {
        return None;
    }
    if let Some(at) = crate::term::float_slot(ty) {
        return Some(["rv.ld_f32", "rv.ld_f64"][at]);
    }
    let names = ["rv.ld_8", "rv.ld_16", "rv.ld_32", "rv.ld_64"];
    Some(names[place(ty, 64)?])
}

/// What the instruction that writes an argument of that type into memory is called.
fn store_of(ty: Type) -> Option<&'static str> {
    if untaught(ty) {
        return None;
    }
    if let Some(at) = crate::term::float_slot(ty) {
        return Some(["rv.st_f32", "rv.st_f64"][at]);
    }
    let names = ["rv.st_8", "rv.st_16", "rv.st_32", "rv.st_64"];
    Some(names[place(ty, 64)?])
}

/// What the pseudo that leaves a returned value in its register is called, for the value at that
/// place in its own register file.
///
/// Two places in each file. LP64D gives back two integer registers, two float registers, or one
/// of each for a structure of an integer and a float.
fn ret_of(ty: Type, at: usize) -> Option<&'static str> {
    if untaught(ty) {
        return None;
    }
    if let Some(width) = crate::term::float_slot(ty) {
        let names = [["rv.ret_val_f32", "rv.ret_val_f64"], ["rv.ret_val2_f32", "rv.ret_val2_f64"]];
        return Some(names.get(at)?[width]);
    }
    let names = [
        ["rv.ret_val_32", "rv.ret_val_32", "rv.ret_val_32", "rv.ret_val_64"],
        ["rv.ret_val2_32", "rv.ret_val2_32", "rv.ret_val2_32", "rv.ret_val2_64"],
    ];
    Some(names.get(at)?[place(ty, 64)?])
}

#[cfg(test)]
mod tests {
    use rucc_ir::{Float, Type};
    use rucc_target::riscv64;

    use super::*;

    /// Every type the four functions answer for, and a few they do not.
    fn types() -> Vec<Type> {
        let mut types: Vec<Type> = [1, 8, 16, 32, 64, 128].into_iter().map(Type::int).collect();
        types
            .extend([Float::F16, Float::F32, Float::F64, Float::F80, Float::F128].map(Type::float));
        types.push(Type::PTR);
        types
    }

    #[test]
    fn every_name_is_an_instruction_the_machine_describes() {
        let described = |name: &str| {
            let bare = name.strip_prefix("rv.").expect("a RISC-V name");
            assert!(riscv64::form(bare).is_some(), "{name}");
        };
        for ty in types() {
            for name in [head_of(ty), load_of(ty), store_of(ty)]
                .into_iter()
                .chain((0..2).map(|at| ret_of(ty, at)))
            {
                name.map(described);
            }
            for abi in [Abi::Sext, Abi::Zext] {
                extend_of(ty, abi).map(described);
            }
        }
        for name in [INSTS.call, INSTS.call_reg, INSTS.lea, INSTS.small] {
            described(name);
        }
    }

    #[test]
    fn the_four_answer_for_the_same_types() {
        for ty in types() {
            let arrives = head_of(ty).is_some();
            assert_eq!(arrives, load_of(ty).is_some(), "{ty:?}");
            assert_eq!(arrives, store_of(ty).is_some(), "{ty:?}");
            assert_eq!(arrives, ret_of(ty, 0).is_some(), "{ty:?}");
        }
    }

    #[test]
    fn a_narrow_integer_travels_as_a_word_and_is_read_at_its_own_width() {
        assert_eq!(head_of(Type::int(8)), Some("rv.arg_val_32"));
        assert_eq!(head_of(Type::int(32)), Some("rv.arg_val_32"));
        assert_eq!(head_of(Type::PTR), Some("rv.arg_val_64"));
        assert_eq!(load_of(Type::int(16)), Some("rv.ld_16"));
        assert_eq!(store_of(Type::int(32)), Some("rv.st_32"));
        assert_eq!(ret_of(Type::int(1), 0), Some("rv.ret_val_32"));
        assert_eq!(ret_of(Type::int(64), 1), Some("rv.ret_val2_64"));
        assert_eq!(ret_of(Type::int(64), 2), None);
        assert_eq!(ret_of(Type::float(Float::F64), 1), Some("rv.ret_val2_f64"));
    }

    #[test]
    fn what_has_no_rules_is_not_passed_yet() {
        assert_eq!(head_of(Type::float(Float::F128)), None);
        assert_eq!(head_of(Type::float(Float::F16)), None);
        assert_eq!(head_of(Type::int(128)), None);
    }
}
