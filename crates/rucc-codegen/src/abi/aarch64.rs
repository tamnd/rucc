//! The instructions the calling convention is written with on AArch64.
//!
//! The same four questions [`super::head_of`] and its neighbours answer for x86-64, answered with
//! the names `rucc_target::aarch64` describes. Two things make the answers shorter here. A value
//! narrower than 32 bits lives in a 32 bit register, so a byte and a short arrive and go back as
//! the 32 bit pseudo, the same as an `int`. And there is no second name for a half: `_Float16`
//! has no rule on this machine yet, so it has no name here either, and a function that passes one
//! is refused at the argument rather than halfway through.

use rucc_ir::Type;

use super::{Insts, place};

/// The AArch64 ones.
pub static INSTS: Insts = Insts {
    arg: head_of,
    load: load_of,
    store: store_of,
    ret: ret_of,
    call: "a64.bl",
    call_reg: "a64.blr",
    lea: "a64.lea_64",
    small: "a64.mov_ri_32",
};

/// What the pseudo for an argument of that type is called.
fn head_of(ty: Type) -> Option<&'static str> {
    if crate::term::is_quad(ty) {
        return Some("a64.arg_val_f128");
    }
    if crate::term::is_half(ty) {
        return None;
    }
    if let Some(at) = crate::term::float_slot(ty) {
        return Some(["a64.arg_val_f32", "a64.arg_val_f64"][at]);
    }
    let names = ["a64.arg_val_32", "a64.arg_val_32", "a64.arg_val_32", "a64.arg_val_64"];
    Some(names[place(ty)?])
}

/// What the instruction that reads an argument of that type out of memory is called.
///
/// A narrow one is read at its own width, for the reason [`super::load_of`] gives: the caller
/// wrote a whole slot and the convention says nothing about what is above the value in it.
fn load_of(ty: Type) -> Option<&'static str> {
    if crate::term::is_quad(ty) {
        return Some("a64.ldr_f128");
    }
    if crate::term::is_half(ty) {
        return None;
    }
    if let Some(at) = crate::term::float_slot(ty) {
        return Some(["a64.ldr_f32", "a64.ldr_f64"][at]);
    }
    let names = ["a64.ldr_8", "a64.ldr_16", "a64.ldr_32", "a64.ldr_64"];
    Some(names[place(ty)?])
}

/// What the instruction that writes an argument of that type into memory is called.
fn store_of(ty: Type) -> Option<&'static str> {
    if crate::term::is_quad(ty) {
        return Some("a64.str_f128");
    }
    if crate::term::is_half(ty) {
        return None;
    }
    if let Some(at) = crate::term::float_slot(ty) {
        return Some(["a64.str_f32", "a64.str_f64"][at]);
    }
    let names = ["a64.str_8", "a64.str_16", "a64.str_32", "a64.str_64"];
    Some(names[place(ty)?])
}

/// What the pseudo that leaves a returned value in its register is called, for the value at that
/// place in its own register file.
///
/// Two places in each file, the same as x86-64. AAPCS64 gives back up to eight registers of either
/// kind, but the front end splits nothing into more than two pieces yet, so a third name would be
/// a name nothing asks for.
fn ret_of(ty: Type, at: usize) -> Option<&'static str> {
    if crate::term::is_quad(ty) {
        return Some(*["a64.ret_val_f128", "a64.ret_val2_f128"].get(at)?);
    }
    if crate::term::is_half(ty) {
        return None;
    }
    if let Some(width) = crate::term::float_slot(ty) {
        let names =
            [["a64.ret_val_f32", "a64.ret_val_f64"], ["a64.ret_val2_f32", "a64.ret_val2_f64"]];
        return Some(names.get(at)?[width]);
    }
    let names = [
        ["a64.ret_val_32", "a64.ret_val_32", "a64.ret_val_32", "a64.ret_val_64"],
        ["a64.ret_val2_32", "a64.ret_val2_32", "a64.ret_val2_32", "a64.ret_val2_64"],
    ];
    Some(names.get(at)?[place(ty)?])
}

#[cfg(test)]
mod tests {
    use rucc_ir::{Float, Type};
    use rucc_target::aarch64;

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
            let bare = name.strip_prefix("a64.").expect("an AArch64 name");
            assert!(aarch64::form(bare).is_some(), "{name}");
        };
        for ty in types() {
            for name in [head_of(ty), load_of(ty), store_of(ty), ret_of(ty, 0), ret_of(ty, 1)] {
                name.map(described);
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
    fn a_narrow_integer_travels_in_a_32_bit_register_and_is_read_at_its_own_width() {
        assert_eq!(head_of(Type::int(8)), Some("a64.arg_val_32"));
        assert_eq!(head_of(Type::int(16)), Some("a64.arg_val_32"));
        assert_eq!(head_of(Type::PTR), Some("a64.arg_val_64"));
        assert_eq!(load_of(Type::int(8)), Some("a64.ldr_8"));
        assert_eq!(store_of(Type::int(16)), Some("a64.str_16"));
        assert_eq!(ret_of(Type::int(1), 0), Some("a64.ret_val_32"));
        assert_eq!(ret_of(Type::int(64), 1), Some("a64.ret_val2_64"));
        assert_eq!(ret_of(Type::int(64), 2), None);
    }

    #[test]
    fn a_long_double_is_a_quad_and_a_half_is_not_passed_yet() {
        assert_eq!(head_of(Type::float(Float::F128)), Some("a64.arg_val_f128"));
        assert_eq!(head_of(Type::float(Float::F16)), None);
        assert_eq!(head_of(Type::int(128)), None);
    }
}
