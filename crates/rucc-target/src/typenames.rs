//! The type names a target's compiler has before any header has been read.
//!
//! gcc registers a handful of names per architecture as if a typedef had declared them at file
//! scope, and headers are written against them. AArch64 glibc is the one that matters: its
//! `bits/math-vector.h`, which `<math.h>` includes, writes `typedef __Float32x4_t __f32x4_t;` when
//! `__GNUC__` is nine or more and `typedef __SVFloat32_t __sv_f32_t;` when it is ten or more. A
//! compiler without them cannot include `<math.h>` for that target at all.
//!
//! They are names and not keywords, as they are in gcc, so a program may declare something of
//! the same name and hide one, and on every other target they are ordinary identifiers.

use rucc_tuple::Arch;

/// What one lane of a vector is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    /// `signed char`.
    I8,
    /// `unsigned char`, which is also the lane of a polynomial of eight bits.
    U8,
    /// `short`.
    I16,
    /// `unsigned short`.
    U16,
    /// `int`.
    I32,
    /// `unsigned int`.
    U32,
    /// `long`, which is sixty four bits on every target these are for.
    I64,
    /// `unsigned long`.
    U64,
    /// `_Float16`.
    F16,
    /// `float`.
    F32,
    /// `double`.
    F64,
}

/// The type one of the names is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeName {
    /// A GNU vector of this many lanes, which is the type `__attribute__((vector_size))` builds.
    Vector(Lane, u8),
    /// One of SVE's types, whose size depends on the machine the program runs on. C has no such
    /// type, and gcc allows one in a declaration and nearly nowhere else, so it is an incomplete
    /// type here: a prototype can name it, and an object of it is refused.
    Sizeless,
}

/// The Advanced SIMD vectors under the names gcc gives them in `aarch64-simd-builtin-types.def`,
/// which `arm_neon.h` builds `float32x4_t` and the rest out of, and then SVE's. The `__Bfloat16`
/// ones are left out until `__bf16` is a type here.
const AARCH64: &[(&str, TypeName)] = &[
    ("__Int8x8_t", TypeName::Vector(Lane::I8, 8)),
    ("__Int16x4_t", TypeName::Vector(Lane::I16, 4)),
    ("__Int32x2_t", TypeName::Vector(Lane::I32, 2)),
    ("__Int64x1_t", TypeName::Vector(Lane::I64, 1)),
    ("__Uint8x8_t", TypeName::Vector(Lane::U8, 8)),
    ("__Uint16x4_t", TypeName::Vector(Lane::U16, 4)),
    ("__Uint32x2_t", TypeName::Vector(Lane::U32, 2)),
    ("__Uint64x1_t", TypeName::Vector(Lane::U64, 1)),
    ("__Poly8x8_t", TypeName::Vector(Lane::U8, 8)),
    ("__Poly16x4_t", TypeName::Vector(Lane::U16, 4)),
    ("__Poly64x1_t", TypeName::Vector(Lane::U64, 1)),
    ("__Float16x4_t", TypeName::Vector(Lane::F16, 4)),
    ("__Float32x2_t", TypeName::Vector(Lane::F32, 2)),
    ("__Float64x1_t", TypeName::Vector(Lane::F64, 1)),
    ("__Int8x16_t", TypeName::Vector(Lane::I8, 16)),
    ("__Int16x8_t", TypeName::Vector(Lane::I16, 8)),
    ("__Int32x4_t", TypeName::Vector(Lane::I32, 4)),
    ("__Int64x2_t", TypeName::Vector(Lane::I64, 2)),
    ("__Uint8x16_t", TypeName::Vector(Lane::U8, 16)),
    ("__Uint16x8_t", TypeName::Vector(Lane::U16, 8)),
    ("__Uint32x4_t", TypeName::Vector(Lane::U32, 4)),
    ("__Uint64x2_t", TypeName::Vector(Lane::U64, 2)),
    ("__Poly8x16_t", TypeName::Vector(Lane::U8, 16)),
    ("__Poly16x8_t", TypeName::Vector(Lane::U16, 8)),
    ("__Poly64x2_t", TypeName::Vector(Lane::U64, 2)),
    ("__Float16x8_t", TypeName::Vector(Lane::F16, 8)),
    ("__Float32x4_t", TypeName::Vector(Lane::F32, 4)),
    ("__Float64x2_t", TypeName::Vector(Lane::F64, 2)),
    ("__SVInt8_t", TypeName::Sizeless),
    ("__SVInt16_t", TypeName::Sizeless),
    ("__SVInt32_t", TypeName::Sizeless),
    ("__SVInt64_t", TypeName::Sizeless),
    ("__SVUint8_t", TypeName::Sizeless),
    ("__SVUint16_t", TypeName::Sizeless),
    ("__SVUint32_t", TypeName::Sizeless),
    ("__SVUint64_t", TypeName::Sizeless),
    ("__SVFloat16_t", TypeName::Sizeless),
    ("__SVFloat32_t", TypeName::Sizeless),
    ("__SVFloat64_t", TypeName::Sizeless),
    ("__SVBool_t", TypeName::Sizeless),
];

/// The names `arch` has before anything is read, and what each one is.
#[must_use]
pub(crate) fn type_names(arch: Arch) -> &'static [(&'static str, TypeName)] {
    match arch {
        Arch::Aarch64 => AARCH64,
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_vector_is_one_of_the_two_widths_the_registers_have() {
        let width = |lane: Lane| match lane {
            Lane::I8 | Lane::U8 => 1,
            Lane::I16 | Lane::U16 | Lane::F16 => 2,
            Lane::I32 | Lane::U32 | Lane::F32 => 4,
            Lane::I64 | Lane::U64 | Lane::F64 => 8,
        };
        for &(name, ty) in AARCH64 {
            if let TypeName::Vector(lane, lanes) = ty {
                let bytes = width(lane) * u32::from(lanes);
                assert!(bytes == 8 || bytes == 16, "{name} is {bytes} bytes");
                let q = name.ends_with(&format!("x{lanes}_t"));
                assert!(q, "{name} does not say it has {lanes} lanes");
            }
        }
    }

    #[test]
    fn only_aarch64_has_any() {
        assert!(!type_names(Arch::Aarch64).is_empty());
        assert!(type_names(Arch::X86_64).is_empty());
    }
}
