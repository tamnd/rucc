//! The Advanced SIMD instructions, which are what gcc writes for a loop it vectorizes and what a
//! program using `arm_neon.h` asks for by name.
//!
//! They come in a few dozen families, and inside a family the instructions differ only in a bit
//! or two and a small opcode field, so each family is one table and one function that reads the
//! operands and fills the word in. The families and their layouts are the ones the Arm ARM gives
//! under "Advanced SIMD" in C4.1, and the words were checked against what GNU as writes for the
//! same lines. See `neon.txt` next to this file.
//!
//! An arrangement says how many lanes of what size a register is read as, `v0.4s` being four of
//! four bytes. Almost every word carries that as two fields, `Q` for whether the whole sixteen
//! bytes are used and `size` for the lane, and the functions here take both from the operands.
//! An instruction whose operands are of different widths, which is every widening and narrowing
//! one, takes its fields from the narrow side, and its name ends in `2` when that narrow side is
//! the upper half of a register rather than the lower.

use super::{Addr, Arrangement, At, Error, Mode, Offset, Scalar, Shift, Value, Width, float_imm8};

/// Which lane sizes an instruction takes, as bits: bytes, halves, words and doublewords. Doubles
/// are the pair `2d`, since the single `1d` is not an arrangement arithmetic is done in.
const B: u8 = 1;
const H: u8 = 2;
const S: u8 = 4;
const D: u8 = 8;
const BHS: u8 = B | H | S;
const ALL: u8 = B | H | S | D;
/// A flag beside the sizes for an instruction that also has a form on one scalar doubleword,
/// `add d0, d1, d2` and `cmeq d0, d1, d2` among them.
const SCALAR: u8 = 16;

/// The scalar forms reach here by name, since their operands are the same `d` registers the
/// floating point instructions take and nothing else tells the two apart.
const SCALAR_NAMES: &[&str] = &[
    "add", "sub", "cmeq", "cmge", "cmgt", "cmhi", "cmhs", "cmle", "cmlt", "cmtst", "sshl", "ushl",
    "srshl", "urshl", "sqadd", "uqadd", "sqsub", "uqsub", "shl", "sshr", "ushr", "ssra", "usra",
    "srshr", "urshr", "srsra", "ursra", "sqshl", "uqshl", "sqshlu", "sli", "sri", "abs", "neg",
    "sqabs", "sqneg", "suqadd",
];

/// Whether an instruction is one of these, which is any that names a vector register with its
/// lanes, an element of one, or a list of them, the moves of a pattern into a vector register,
/// and the integer instructions on one scalar doubleword.
pub(super) fn wanted(mnemonic: &str, values: &[Value]) -> bool {
    values
        .iter()
        .any(|value| matches!(value, Value::Vector(..) | Value::Element(..) | Value::List(..)))
        || matches!(mnemonic, "movi" | "mvni")
        || (matches!(values.first(), Some(Value::Fp(Scalar::D, _)))
            && SCALAR_NAMES.contains(&mnemonic))
        || (matches!(values, [Value::Fp(Scalar::S | Scalar::D, _), Value::Fp(..), ..])
            && FP_SCALAR_NAMES.contains(&mnemonic))
}

/// The floating point instructions on one scalar that are Advanced SIMD ones, which the
/// conversions between two registers of the same kind are, and the comparisons that write a mask.
/// The rest of what a scalar floating point register can be given, `fadd s0, s1, s2` and `fabs`
/// among them, is in the encoder above.
const FP_SCALAR_NAMES: &[&str] = &[
    "fcmeq", "fcmge", "fcmgt", "fcmle", "fcmlt", "facge", "facgt", "fabd", "fmulx", "frecps",
    "frsqrts", "frecpe", "frsqrte", "fcvtns", "fcvtnu", "fcvtms", "fcvtmu", "fcvtps", "fcvtpu",
    "fcvtzs", "fcvtzu", "fcvtas", "fcvtau", "scvtf", "ucvtf",
];

/// One floating point scalar in the precision an instruction on one takes, and whether it is a
/// double, which is the low bit of the size field.
fn precision(scalars: &[Scalar]) -> Option<u32> {
    let first = scalars[0];
    if scalars.iter().any(|&other| other != first) {
        return None;
    }
    match first {
        Scalar::S => Some(0),
        Scalar::D => Some(1),
        _ => None,
    }
}

impl Arrangement {
    /// Whether all sixteen bytes are used, which is the `Q` bit.
    fn q(self) -> u32 {
        u32::from(matches!(
            self,
            Arrangement::B16 | Arrangement::H8 | Arrangement::S4 | Arrangement::D2
        ))
    }

    /// The lane size, as the `size` field has it: bytes are zero and doublewords three.
    fn size(self) -> u32 {
        match self {
            Arrangement::B8 | Arrangement::B16 => 0,
            Arrangement::H4 | Arrangement::H8 => 1,
            Arrangement::S2 | Arrangement::S4 => 2,
            Arrangement::D1 | Arrangement::D2 => 3,
        }
    }

    /// Whether an instruction that takes the lane sizes in `sizes` takes this one.
    fn takes(self, sizes: u8) -> bool {
        self != Arrangement::D1 && sizes & (1 << self.size()) != 0
    }

    /// The whole register of lanes twice as wide as these, which is what a widening instruction
    /// writes and a narrowing one reads.
    fn wide(self) -> Option<Arrangement> {
        match self.size() {
            0 => Some(Arrangement::H8),
            1 => Some(Arrangement::S4),
            2 => Some(Arrangement::D2),
            _ => None,
        }
    }
}

impl Scalar {
    /// The lane size of an element or a scalar, the way [`Arrangement::size`] has it.
    fn size(self) -> Option<u32> {
        match self {
            Scalar::B => Some(0),
            Scalar::H => Some(1),
            Scalar::S => Some(2),
            Scalar::D => Some(3),
            Scalar::Q => None,
        }
    }
}

/// A name with the `2` that says the narrow operand is the upper half taken off, and whether it
/// was there.
fn upper(mnemonic: &str) -> (&str, u32) {
    match mnemonic.strip_suffix('2') {
        Some(base) => (base, 1),
        None => (mnemonic, 0),
    }
}

/// Where an element is in the five bits the copies carry it in: the lane size as the lowest set
/// bit and the index above it.
fn imm5(size: u32, index: u8) -> Option<u32> {
    let index = u32::from(index);
    (index < 16 >> size).then(|| (index << (size + 1)) | (1 << size))
}

/// The bits an element of the third operand is carried in by the instructions that take one, which
/// spread the index over `H`, `L` and `M` and leave the register what is left. A lane of two bytes
/// has eight places and only sixteen registers to be in, so `M` is the index there.
fn by_element(size: u32, reg: u8, index: u8) -> Option<u32> {
    let (reg, index) = (u32::from(reg), u32::from(index));
    match size {
        1 if reg < 16 && index < 8 => {
            Some((index >> 2) << 11 | ((index >> 1) & 1) << 21 | (index & 1) << 20 | reg << 16)
        }
        2 if index < 4 => Some((index >> 1) << 11 | (index & 1) << 21 | reg << 16),
        3 if index < 2 => Some(index << 11 | reg << 16),
        _ => None,
    }
}

/// The eight bits `movi` on doublewords carries a pattern in, one for each byte, which has to be
/// all ones or all zeros.
fn byte_mask(value: u64) -> Option<u32> {
    let mut imm8 = 0;
    for byte in 0..8 {
        match (value >> (byte * 8)) & 0xff {
            0 => {}
            0xff => imm8 |= 1 << byte,
            _ => return None,
        }
    }
    Some(imm8)
}

/// Three registers of the same arrangement, the integer ones: name, `U`, opcode, lane sizes.
const THREE_SAME: [(&str, u32, u32, u8); 38] = [
    ("add", 0, 0b10000, ALL | SCALAR),
    ("sub", 1, 0b10000, ALL | SCALAR),
    ("mul", 0, 0b10011, BHS),
    ("mla", 0, 0b10010, BHS),
    ("mls", 1, 0b10010, BHS),
    ("pmul", 1, 0b10011, B),
    ("cmgt", 0, 0b00110, ALL | SCALAR),
    ("cmge", 0, 0b00111, ALL | SCALAR),
    ("cmhi", 1, 0b00110, ALL | SCALAR),
    ("cmhs", 1, 0b00111, ALL | SCALAR),
    ("cmeq", 1, 0b10001, ALL | SCALAR),
    ("cmtst", 0, 0b10001, ALL | SCALAR),
    ("smax", 0, 0b01100, BHS),
    ("smin", 0, 0b01101, BHS),
    ("umax", 1, 0b01100, BHS),
    ("umin", 1, 0b01101, BHS),
    ("smaxp", 0, 0b10100, BHS),
    ("sminp", 0, 0b10101, BHS),
    ("umaxp", 1, 0b10100, BHS),
    ("uminp", 1, 0b10101, BHS),
    ("addp", 0, 0b10111, ALL),
    ("sshl", 0, 0b01000, ALL | SCALAR),
    ("ushl", 1, 0b01000, ALL | SCALAR),
    ("srshl", 0, 0b01010, ALL | SCALAR),
    ("urshl", 1, 0b01010, ALL | SCALAR),
    ("sqadd", 0, 0b00001, ALL | SCALAR),
    ("uqadd", 1, 0b00001, ALL | SCALAR),
    ("sqsub", 0, 0b00101, ALL | SCALAR),
    ("uqsub", 1, 0b00101, ALL | SCALAR),
    ("shadd", 0, 0b00000, BHS),
    ("uhadd", 1, 0b00000, BHS),
    ("srhadd", 0, 0b00010, BHS),
    ("urhadd", 1, 0b00010, BHS),
    ("sabd", 0, 0b01110, BHS),
    ("uabd", 1, 0b01110, BHS),
    ("saba", 0, 0b01111, BHS),
    ("uaba", 1, 0b01111, BHS),
    ("sqdmulh", 0, 0b10110, H | S),
];

/// The bitwise ones, which only take bytes and use the size field as more opcode: name, `U`, size.
const LOGICAL: [(&str, u32, u32); 8] = [
    ("and", 0, 0),
    ("bic", 0, 1),
    ("orr", 0, 2),
    ("orn", 0, 3),
    ("eor", 1, 0),
    ("bsl", 1, 1),
    ("bit", 1, 2),
    ("bif", 1, 3),
];

/// Three registers of the same arrangement, the floating point ones: name, `U`, the high bit of
/// the size field, and the opcode. The low bit of the size field is the precision.
const FP_THREE: [(&str, u32, u32, u32); 24] = [
    ("fadd", 0, 0, 0b11010),
    ("fsub", 0, 1, 0b11010),
    ("fmul", 1, 0, 0b11011),
    ("fdiv", 1, 0, 0b11111),
    ("fmla", 0, 0, 0b11001),
    ("fmls", 0, 1, 0b11001),
    ("fmax", 0, 0, 0b11110),
    ("fmin", 0, 1, 0b11110),
    ("fmaxnm", 0, 0, 0b11000),
    ("fminnm", 0, 1, 0b11000),
    ("fcmeq", 0, 0, 0b11100),
    ("fcmge", 1, 0, 0b11100),
    ("fcmgt", 1, 1, 0b11100),
    ("facge", 1, 0, 0b11101),
    ("facgt", 1, 1, 0b11101),
    ("fabd", 1, 1, 0b11010),
    ("faddp", 1, 0, 0b11010),
    ("fmaxp", 1, 0, 0b11110),
    ("fminp", 1, 1, 0b11110),
    ("fmaxnmp", 1, 0, 0b11000),
    ("fminnmp", 1, 1, 0b11000),
    ("fmulx", 0, 0, 0b11011),
    ("frecps", 0, 0, 0b11111),
    ("frsqrts", 0, 1, 0b11111),
];

/// One register into another of the same arrangement, the integer ones: name, `U`, opcode, sizes.
const MISC: [(&str, u32, u32, u8); 13] = [
    ("rev64", 0, 0b00000, BHS),
    ("rev32", 1, 0b00000, B | H),
    ("rev16", 0, 0b00001, B),
    ("cls", 0, 0b00100, BHS),
    ("clz", 1, 0b00100, BHS),
    ("cnt", 0, 0b00101, B),
    ("not", 1, 0b00101, B),
    ("mvn", 1, 0b00101, B),
    ("abs", 0, 0b01011, ALL | SCALAR),
    ("neg", 1, 0b01011, ALL | SCALAR),
    ("sqabs", 0, 0b00111, ALL | SCALAR),
    ("sqneg", 1, 0b00111, ALL | SCALAR),
    ("suqadd", 0, 0b00011, ALL | SCALAR),
];

/// The comparisons with zero, integer: name, `U`, opcode.
const MISC_ZERO: [(&str, u32, u32); 5] = [
    ("cmgt", 0, 0b01000),
    ("cmeq", 0, 0b01001),
    ("cmlt", 0, 0b01010),
    ("cmge", 1, 0b01000),
    ("cmle", 1, 0b01001),
];

/// One register into another of the same arrangement, floating point: name, `U`, the high bit of
/// the size field, opcode.
const FP_MISC: [(&str, u32, u32, u32); 24] = [
    ("fabs", 0, 1, 0b01111),
    ("fneg", 1, 1, 0b01111),
    ("fsqrt", 1, 1, 0b11111),
    ("frintn", 0, 0, 0b11000),
    ("frintm", 0, 0, 0b11001),
    ("frintp", 0, 1, 0b11000),
    ("frintz", 0, 1, 0b11001),
    ("frinta", 1, 0, 0b11000),
    ("frintx", 1, 0, 0b11001),
    ("frinti", 1, 1, 0b11001),
    ("fcvtns", 0, 0, 0b11010),
    ("fcvtnu", 1, 0, 0b11010),
    ("fcvtms", 0, 0, 0b11011),
    ("fcvtmu", 1, 0, 0b11011),
    ("fcvtps", 0, 1, 0b11010),
    ("fcvtpu", 1, 1, 0b11010),
    ("fcvtzs", 0, 1, 0b11011),
    ("fcvtzu", 1, 1, 0b11011),
    ("fcvtas", 0, 0, 0b11100),
    ("fcvtau", 1, 0, 0b11100),
    ("scvtf", 0, 0, 0b11101),
    ("ucvtf", 1, 0, 0b11101),
    ("frecpe", 0, 1, 0b11101),
    ("frsqrte", 1, 1, 0b11101),
];

/// The floating point comparisons with zero: name, `U`, opcode. The high size bit is always one.
const FP_ZERO: [(&str, u32, u32); 5] = [
    ("fcmgt", 0, 0b01100),
    ("fcmeq", 0, 0b01101),
    ("fcmlt", 0, 0b01110),
    ("fcmge", 1, 0b01100),
    ("fcmle", 1, 0b01101),
];

/// The narrowing moves, whose destination is half the width of the source: name, `U`, opcode.
const NARROW: [(&str, u32, u32); 4] =
    [("xtn", 0, 0b10010), ("sqxtn", 0, 0b10100), ("uqxtn", 1, 0b10100), ("sqxtun", 1, 0b10010)];

/// The pairwise widening adds, whose destination has half as many lanes twice as wide.
const PAIRWISE_LONG: [(&str, u32, u32); 4] = [
    ("saddlp", 0, 0b00010),
    ("uaddlp", 1, 0b00010),
    ("sadalp", 0, 0b00110),
    ("uadalp", 1, 0b00110),
];

/// Shifts by a number, where the lanes stay the width they are: name, `U`, opcode, and whether
/// it shifts left, which decides how the amount is carried.
const SHIFTS: [(&str, u32, u32, bool); 14] = [
    ("sshr", 0, 0b00000, false),
    ("ushr", 1, 0b00000, false),
    ("ssra", 0, 0b00010, false),
    ("usra", 1, 0b00010, false),
    ("srshr", 0, 0b00100, false),
    ("urshr", 1, 0b00100, false),
    ("srsra", 0, 0b00110, false),
    ("ursra", 1, 0b00110, false),
    ("shl", 0, 0b01010, true),
    ("sli", 1, 0b01010, true),
    ("sri", 1, 0b01000, false),
    ("sqshl", 0, 0b01110, true),
    ("uqshl", 1, 0b01110, true),
    ("sqshlu", 1, 0b01100, true),
];

/// Shifts right that narrow the lanes as they go: name, `U`, opcode.
const SHIFT_NARROW: [(&str, u32, u32); 8] = [
    ("shrn", 0, 0b10000),
    ("rshrn", 0, 0b10001),
    ("sqshrn", 0, 0b10010),
    ("uqshrn", 1, 0b10010),
    ("sqrshrn", 0, 0b10011),
    ("uqrshrn", 1, 0b10011),
    ("sqshrun", 1, 0b10000),
    ("sqrshrun", 1, 0b10001),
];

/// Conversions between integers and fixed point numbers of a given number of fraction bits,
/// which are shifts right to the encoding: name, `U`, opcode.
const FIXED: [(&str, u32, u32); 4] =
    [("scvtf", 0, 0b11100), ("ucvtf", 1, 0b11100), ("fcvtzs", 0, 0b11111), ("fcvtzu", 1, 0b11111)];

/// How the operands of an instruction on three registers of different widths are laid out.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// Two narrow sources into a wide destination, `saddl v0.2d, v1.2s, v2.2s`.
    Long,
    /// A wide source and a narrow one into a wide destination, `saddw v0.2d, v1.2d, v2.2s`.
    Wide,
    /// Two wide sources into a narrow destination, `addhn v0.2s, v1.2d, v2.2d`.
    Narrow,
}

/// The instructions on three registers of different widths: name, `U`, opcode, shape.
const THREE_DIFFERENT: [(&str, u32, u32, Shape); 26] = [
    ("saddl", 0, 0b0000, Shape::Long),
    ("uaddl", 1, 0b0000, Shape::Long),
    ("saddw", 0, 0b0001, Shape::Wide),
    ("uaddw", 1, 0b0001, Shape::Wide),
    ("ssubl", 0, 0b0010, Shape::Long),
    ("usubl", 1, 0b0010, Shape::Long),
    ("ssubw", 0, 0b0011, Shape::Wide),
    ("usubw", 1, 0b0011, Shape::Wide),
    ("addhn", 0, 0b0100, Shape::Narrow),
    ("raddhn", 1, 0b0100, Shape::Narrow),
    ("sabal", 0, 0b0101, Shape::Long),
    ("uabal", 1, 0b0101, Shape::Long),
    ("subhn", 0, 0b0110, Shape::Narrow),
    ("rsubhn", 1, 0b0110, Shape::Narrow),
    ("sabdl", 0, 0b0111, Shape::Long),
    ("uabdl", 1, 0b0111, Shape::Long),
    ("smlal", 0, 0b1000, Shape::Long),
    ("umlal", 1, 0b1000, Shape::Long),
    ("sqdmlal", 0, 0b1001, Shape::Long),
    ("smlsl", 0, 0b1010, Shape::Long),
    ("umlsl", 1, 0b1010, Shape::Long),
    ("sqdmlsl", 0, 0b1011, Shape::Long),
    ("smull", 0, 0b1100, Shape::Long),
    ("umull", 1, 0b1100, Shape::Long),
    ("sqdmull", 0, 0b1101, Shape::Long),
    ("pmull", 0, 0b1110, Shape::Long),
];

/// The permutes: name and opcode.
const PERMUTE: [(&str, u32); 6] =
    [("uzp1", 1), ("trn1", 2), ("zip1", 3), ("uzp2", 5), ("trn2", 6), ("zip2", 7)];

/// The reductions across the lanes of one register into a scalar: name, `U`, opcode, and whether
/// the scalar is twice the width of a lane.
const ACROSS: [(&str, u32, u32, bool); 7] = [
    ("saddlv", 0, 0b00011, true),
    ("uaddlv", 1, 0b00011, true),
    ("smaxv", 0, 0b01010, false),
    ("umaxv", 1, 0b01010, false),
    ("sminv", 0, 0b11010, false),
    ("uminv", 1, 0b11010, false),
    ("addv", 0, 0b11011, false),
];

/// The floating point reductions and the pairwise ones into a scalar: name, the high size bit,
/// opcode.
const FP_ACROSS: [(&str, u32, u32); 4] = [
    ("fmaxnmv", 0, 0b01100),
    ("fmaxv", 0, 0b01111),
    ("fminnmv", 1, 0b01100),
    ("fminv", 1, 0b01111),
];
const FP_PAIR: [(&str, u32, u32); 5] = [
    ("faddp", 0, 0b01101),
    ("fmaxnmp", 0, 0b01100),
    ("fmaxp", 0, 0b01111),
    ("fminnmp", 1, 0b01100),
    ("fminp", 1, 0b01111),
];

/// Multiplies by one element of a register, same width: name, `U`, opcode.
const BY_ELEMENT: [(&str, u32, u32); 5] = [
    ("mul", 0, 0b1000),
    ("mla", 1, 0b0000),
    ("mls", 1, 0b0100),
    ("sqdmulh", 0, 0b1100),
    ("sqrdmulh", 0, 0b1101),
];
/// The widening ones: name, `U`, opcode.
const BY_ELEMENT_LONG: [(&str, u32, u32); 6] = [
    ("smull", 0, 0b1010),
    ("umull", 1, 0b1010),
    ("smlal", 0, 0b0010),
    ("umlal", 1, 0b0010),
    ("smlsl", 0, 0b0110),
    ("umlsl", 1, 0b0110),
];
/// The floating point ones: name, `U`, opcode.
const FP_BY_ELEMENT: [(&str, u32, u32); 4] =
    [("fmla", 0, 0b0001), ("fmls", 0, 0b0101), ("fmul", 0, 0b1001), ("fmulx", 1, 0b1001)];

/// One family of instructions: the word when the instruction is one of them, nothing when it is
/// not, and an error when it is one of them with operands it cannot take.
type Family<'a> = fn(At<'a>, &[Value]) -> Result<Option<u32>, Error>;

/// A table row by name.
fn row<T: Copy>(table: &[(&str, T)], name: &str) -> Option<T> {
    table.iter().find(|entry| entry.0 == name).map(|entry| entry.1)
}

impl At<'_> {
    /// The word for one of these, or why there is none.
    pub(super) fn neon(self, values: &[Value]) -> Result<u32, Error> {
        let families: [Family<'_>; 12] = [
            At::copy,
            At::modified,
            At::three_same,
            At::fp_three,
            At::misc,
            At::widths,
            At::shifts,
            At::three_different,
            At::permute,
            At::reduce,
            At::element,
            At::structures,
        ];
        for family in families {
            if let Some(word) = family(self, values)? {
                return Ok(word);
            }
        }
        Err(self.unwritten())
    }

    /// Every lane arrangement the same and one the instruction takes.
    fn alike(self, all: &[Arrangement], sizes: u8) -> Result<Arrangement, Error> {
        let first = all[0];
        if all.iter().any(|&other| other != first) || !first.takes(sizes) {
            return Err(self.register());
        }
        Ok(first)
    }

    /// The copies between elements, whole registers and general registers.
    fn copy(self, values: &[Value]) -> Result<Option<u32>, Error> {
        let m = self.mnemonic;
        let at =
            |size: u32, index: u8| imm5(size, index).ok_or_else(|| self.immediate(index.into()));
        let esize = |scalar: Scalar| scalar.size().ok_or_else(|| self.register());
        let word = match (m, values) {
            ("dup", [Value::Vector(a, d), Value::Element(e, n, i)]) => {
                if *a == Arrangement::D1 || a.size() != esize(*e)? {
                    return Err(self.register());
                }
                0x0E00_0400
                    | a.q() << 30
                    | at(a.size(), *i)? << 16
                    | u32::from(*n) << 5
                    | u32::from(*d)
            }
            ("dup", [Value::Vector(a, d), Value::Gpr(width, n)]) => {
                if *a == Arrangement::D1 || (*width == Width::X) != (a.size() == 3) {
                    return Err(self.register());
                }
                0x0E00_0C00
                    | a.q() << 30
                    | (1 << a.size()) << 16
                    | u32::from(*n) << 5
                    | u32::from(*d)
            }
            ("dup" | "mov", [Value::Fp(s, d), Value::Element(e, n, i)]) => {
                if s != e {
                    return Err(self.register());
                }
                0x5E00_0400 | at(esize(*e)?, *i)? << 16 | u32::from(*n) << 5 | u32::from(*d)
            }
            ("ins" | "mov", [Value::Element(e, d, i), Value::Gpr(width, n)]) => {
                let size = esize(*e)?;
                if (*width == Width::X) != (size == 3) {
                    return Err(self.register());
                }
                0x4E00_1C00 | at(size, *i)? << 16 | u32::from(*n) << 5 | u32::from(*d)
            }
            ("ins" | "mov", [Value::Element(e, d, i), Value::Element(f, n, j)]) => {
                if e != f {
                    return Err(self.register());
                }
                let size = esize(*e)?;
                let from = u32::from(*j);
                if from >= 16 >> size {
                    return Err(self.immediate((*j).into()));
                }
                0x6E00_0400
                    | at(size, *i)? << 16
                    | (from << size) << 11
                    | u32::from(*n) << 5
                    | u32::from(*d)
            }
            ("umov" | "mov", [Value::Gpr(width, d), Value::Element(e, n, i)]) => {
                let size = esize(*e)?;
                // `mov` is only the name of the word for the lane sizes that fill the register.
                let fills = matches!((size, width), (2, Width::W) | (3, Width::X));
                if (*width == Width::X) != (size == 3) || (m == "mov" && !fills) {
                    return Err(self.register());
                }
                0x0E00_3C00
                    | u32::from(size == 3) << 30
                    | at(size, *i)? << 16
                    | u32::from(*n) << 5
                    | u32::from(*d)
            }
            ("smov", [Value::Gpr(width, d), Value::Element(e, n, i)]) => {
                let size = esize(*e)?;
                if size == 3 || (size == 2 && *width == Width::W) {
                    return Err(self.register());
                }
                0x0E00_2C00
                    | u32::from(*width == Width::X) << 30
                    | at(size, *i)? << 16
                    | u32::from(*n) << 5
                    | u32::from(*d)
            }
            // The upper half of a register and a general one, which is how a program moves a
            // sixteen byte number in and out in two pieces.
            ("fmov", [Value::Gpr(Width::X, d), Value::Element(Scalar::D, n, 1)]) => {
                0x9EAE_0000 | u32::from(*n) << 5 | u32::from(*d)
            }
            ("fmov", [Value::Element(Scalar::D, d, 1), Value::Gpr(Width::X, n)]) => {
                0x9EAF_0000 | u32::from(*n) << 5 | u32::from(*d)
            }
            _ => return Ok(None),
        };
        Ok(Some(word))
    }

    /// A number or a pattern moved into every lane, and the logical ones with a number: `movi`,
    /// `mvni`, `orr`, `bic` and `fmov`. The eight bits of the number are split three and five
    /// around a field that says how they are placed in a lane.
    fn modified(self, values: &[Value]) -> Result<Option<u32>, Error> {
        let m = self.mnemonic;
        let (q, d, imm, rest) = match values {
            [Value::Vector(a, d), Value::Imm(imm), rest @ ..] => (Some(*a), *d, *imm, rest),
            [Value::Fp(Scalar::D, d), Value::Imm(imm)] if m == "movi" => (None, *d, *imm, &[][..]),
            [Value::Vector(a, d), Value::Float(number)] if m == "fmov" => {
                let imm8 = float_imm8(*number).ok_or_else(|| self.unwritten())?;
                let (op, q) = match a {
                    Arrangement::S2 | Arrangement::S4 => (0, a.q()),
                    Arrangement::D2 => (1, 1),
                    _ => return Err(self.register()),
                };
                return Ok(Some(modified(q, op, 0b1111, imm8, *d)));
            }
            _ => return Ok(None),
        };
        let shift = match rest {
            [] => 0,
            [Value::Shift(Shift::Lsl, amount)] => u32::from(*amount),
            _ => return Err(self.unwritten()),
        };
        let invert = u32::from(matches!(m, "mvni" | "bic"));
        let logical = u32::from(matches!(m, "orr" | "bic"));
        if !matches!(m, "movi" | "mvni" | "orr" | "bic") {
            return Ok(None);
        }
        // A byte with its top bit set may come as a negative number, which is how gcc writes one:
        // `movi v0.16b, 0xffffffffffffff83` is the byte 0x83. GNU as takes that in every lane size.
        let byte = |imm: i64| (-128..256).contains(&imm).then_some(imm as u32 & 0xff);
        let word = match q {
            // A doubleword pattern, where each bit of the eight says whether a byte is all ones.
            None | Some(Arrangement::D2) if m == "movi" && shift == 0 => {
                let imm8 = byte_mask(imm as u64).ok_or_else(|| self.immediate(imm))?;
                modified(u32::from(q.is_some()), 1, 0b1110, imm8, d)
            }
            Some(a @ (Arrangement::B8 | Arrangement::B16)) if m == "movi" && shift == 0 => {
                let imm8 = byte(imm).ok_or_else(|| self.immediate(imm))?;
                modified(a.q(), 0, 0b1110, imm8, d)
            }
            Some(a @ (Arrangement::H4 | Arrangement::H8)) if matches!(shift, 0 | 8) => {
                let imm8 = byte(imm).ok_or_else(|| self.immediate(imm))?;
                modified(a.q(), invert, 0b1000 | (shift / 8) << 1 | logical, imm8, d)
            }
            Some(a @ (Arrangement::S2 | Arrangement::S4)) if matches!(shift, 0 | 8 | 16 | 24) => {
                let imm8 = byte(imm).ok_or_else(|| self.immediate(imm))?;
                modified(a.q(), invert, (shift / 8) << 1 | logical, imm8, d)
            }
            _ => return Err(self.immediate(imm)),
        };
        Ok(Some(word))
    }

    /// Three registers of one arrangement, integer, and the bitwise ones.
    fn three_same(self, values: &[Value]) -> Result<Option<u32>, Error> {
        let m = self.mnemonic;
        if let Some((u, size)) = row(&LOGICAL.map(|(name, u, size)| (name, (u, size))), m) {
            return Ok(match values {
                [Value::Vector(a, d), Value::Vector(b, n), Value::Vector(c, r)] => {
                    let a = self.alike(&[*a, *b, *c], B)?;
                    Some(
                        0x0E20_1C00
                            | a.q() << 30
                            | u << 29
                            | size << 22
                            | u32::from(*r) << 16
                            | u32::from(*n) << 5
                            | u32::from(*d),
                    )
                }
                _ => None,
            });
        }
        if m == "mov" {
            // `orr` with the same register twice, which is how a whole vector register is copied.
            return Ok(match values {
                [Value::Vector(a, d), Value::Vector(b, n)] => {
                    let a = self.alike(&[*a, *b], B)?;
                    let n = u32::from(*n);
                    Some(0x0EA0_1C00 | a.q() << 30 | n << 16 | n << 5 | u32::from(*d))
                }
                _ => None,
            });
        }
        let Some((u, opcode, sizes)) =
            row(&THREE_SAME.map(|(name, u, opcode, sizes)| (name, (u, opcode, sizes))), m)
        else {
            return Ok(None);
        };
        let word = match values {
            [Value::Vector(a, d), Value::Vector(b, n), Value::Vector(c, r)] => {
                let a = self.alike(&[*a, *b, *c], sizes)?;
                0x0E20_0400
                    | a.q() << 30
                    | u << 29
                    | a.size() << 22
                    | u32::from(*r) << 16
                    | opcode << 11
                    | u32::from(*n) << 5
                    | u32::from(*d)
            }
            [Value::Fp(Scalar::D, d), Value::Fp(Scalar::D, n), Value::Fp(Scalar::D, r)]
                if sizes & SCALAR != 0 =>
            {
                0x5EE0_0400
                    | u << 29
                    | u32::from(*r) << 16
                    | opcode << 11
                    | u32::from(*n) << 5
                    | u32::from(*d)
            }
            _ => return Ok(None),
        };
        Ok(Some(word))
    }

    /// Three registers of one arrangement, floating point.
    fn fp_three(self, values: &[Value]) -> Result<Option<u32>, Error> {
        let Some((u, high, opcode)) =
            row(&FP_THREE.map(|(name, u, high, opcode)| (name, (u, high, opcode))), self.mnemonic)
        else {
            return Ok(None);
        };
        if let [Value::Fp(a, d), Value::Fp(b, n), Value::Fp(c, r)] = values {
            if !FP_SCALAR_NAMES.contains(&self.mnemonic) {
                return Ok(None);
            }
            let double = precision(&[*a, *b, *c]).ok_or_else(|| self.register())?;
            return Ok(Some(
                0x5E20_0400
                    | u << 29
                    | high << 23
                    | double << 22
                    | u32::from(*r) << 16
                    | opcode << 11
                    | u32::from(*n) << 5
                    | u32::from(*d),
            ));
        }
        let [Value::Vector(a, d), Value::Vector(b, n), Value::Vector(c, r)] = values else {
            return Ok(None);
        };
        let a = self.alike(&[*a, *b, *c], S | D)?;
        Ok(Some(
            0x0E20_0400
                | a.q() << 30
                | u << 29
                | high << 23
                | u32::from(a.size() == 3) << 22
                | u32::from(*r) << 16
                | opcode << 11
                | u32::from(*n) << 5
                | u32::from(*d),
        ))
    }

    /// One register into another of the same arrangement, and the comparisons with zero.
    fn misc(self, values: &[Value]) -> Result<Option<u32>, Error> {
        let m = self.mnemonic;
        let misc = |q: u32, u: u32, size: u32, opcode: u32, n: u8, d: u8| {
            0x0E20_0800
                | q << 30
                | u << 29
                | size << 22
                | opcode << 12
                | u32::from(n) << 5
                | u32::from(d)
        };
        // Scalar forms set bit twenty eight and the whole register bit, with the doubleword size.
        let scalar = |u: u32, opcode: u32, n: u8, d: u8| misc(1, u, 3, opcode, n, d) | 0x1000_0000;
        let zero = matches!(values.get(2), Some(Value::Imm(0)))
            || matches!(values.get(2), Some(Value::Float(number)) if *number == 0.0);
        if m == "rbit" {
            return Ok(match values {
                [Value::Vector(a, d), Value::Vector(b, n)] => {
                    let a = self.alike(&[*a, *b], B)?;
                    Some(misc(a.q(), 1, 1, 0b00101, *n, *d))
                }
                _ => None,
            });
        }
        if let Some((u, opcode, sizes)) =
            row(&MISC.map(|(name, u, opcode, sizes)| (name, (u, opcode, sizes))), m)
        {
            match values {
                [Value::Vector(a, d), Value::Vector(b, n)] => {
                    let a = self.alike(&[*a, *b], sizes)?;
                    return Ok(Some(misc(a.q(), u, a.size(), opcode, *n, *d)));
                }
                [Value::Fp(Scalar::D, d), Value::Fp(Scalar::D, n)] if sizes & SCALAR != 0 => {
                    return Ok(Some(scalar(u, opcode, *n, *d)));
                }
                _ => {}
            }
        }
        if let Some((u, opcode)) = row(&MISC_ZERO.map(|(name, u, opcode)| (name, (u, opcode))), m) {
            match values {
                [Value::Vector(a, d), Value::Vector(b, n), _] if zero => {
                    let a = self.alike(&[*a, *b], ALL)?;
                    return Ok(Some(misc(a.q(), u, a.size(), opcode, *n, *d)));
                }
                [Value::Fp(Scalar::D, d), Value::Fp(Scalar::D, n), _] if zero => {
                    return Ok(Some(scalar(u, opcode, *n, *d)));
                }
                _ => {}
            }
        }
        let fp = |q: u32, u: u32, high: u32, double: bool, opcode: u32, n: u8, d: u8| {
            misc(q, u, high << 1 | u32::from(double), opcode, n, d)
        };
        if let Some((u, high, opcode)) =
            row(&FP_MISC.map(|(name, u, high, opcode)| (name, (u, high, opcode))), m)
        {
            match values {
                [Value::Vector(a, d), Value::Vector(b, n)] => {
                    let a = self.alike(&[*a, *b], S | D)?;
                    return Ok(Some(fp(a.q(), u, high, a.size() == 3, opcode, *n, *d)));
                }
                [Value::Fp(a, d), Value::Fp(b, n)] if FP_SCALAR_NAMES.contains(&m) => {
                    let double = precision(&[*a, *b]).ok_or_else(|| self.register())?;
                    return Ok(Some(fp(1, u, high, double == 1, opcode, *n, *d) | 0x1000_0000));
                }
                _ => {}
            }
        }
        if let Some((u, opcode)) = row(&FP_ZERO.map(|(name, u, opcode)| (name, (u, opcode))), m) {
            match values {
                [Value::Vector(a, d), Value::Vector(b, n), _] if zero => {
                    let a = self.alike(&[*a, *b], S | D)?;
                    return Ok(Some(fp(a.q(), u, 1, a.size() == 3, opcode, *n, *d)));
                }
                [Value::Fp(a, d), Value::Fp(b, n), _] if zero => {
                    let double = precision(&[*a, *b]).ok_or_else(|| self.register())?;
                    return Ok(Some(fp(1, u, 1, double == 1, opcode, *n, *d) | 0x1000_0000));
                }
                _ => {}
            }
        }
        Ok(None)
    }

    /// The instructions that change the width of the lanes on one register: the narrowing moves,
    /// the pairwise widening adds, and the conversions between the two floating point widths.
    fn widths(self, values: &[Value]) -> Result<Option<u32>, Error> {
        let [Value::Vector(a, d), Value::Vector(b, n)] = values else {
            return Ok(None);
        };
        let (base, second) = upper(self.mnemonic);
        let (d, n) = (u32::from(*d), u32::from(*n));
        let misc = |q: u32, u: u32, size: u32, opcode: u32| {
            0x0E20_0800 | q << 30 | u << 29 | size << 22 | opcode << 12 | n << 5 | d
        };
        if let Some((u, opcode)) = row(&NARROW.map(|(name, u, opcode)| (name, (u, opcode))), base) {
            if a.q() != second || a.size() == 3 || b.size() != a.size() + 1 || b.q() != 1 {
                return Err(self.register());
            }
            return Ok(Some(misc(a.q(), u, a.size(), opcode)));
        }
        if let Some((u, opcode)) =
            row(&PAIRWISE_LONG.map(|(name, u, opcode)| (name, (u, opcode))), self.mnemonic)
        {
            if b.size() == 3 || a.size() != b.size() + 1 || a.q() != b.q() {
                return Err(self.register());
            }
            return Ok(Some(misc(b.q(), u, b.size(), opcode)));
        }
        let word = match (base, a, b) {
            // Half precision to single, or single to double, from the lower or the upper half.
            ("fcvtl", Arrangement::S4, Arrangement::H4 | Arrangement::H8) => {
                misc(b.q(), 0, 0, 0b10111)
            }
            ("fcvtl", Arrangement::D2, Arrangement::S2 | Arrangement::S4) => {
                misc(b.q(), 0, 1, 0b10111)
            }
            ("fcvtn", Arrangement::H4 | Arrangement::H8, Arrangement::S4) => {
                misc(a.q(), 0, 0, 0b10110)
            }
            ("fcvtn", Arrangement::S2 | Arrangement::S4, Arrangement::D2) => {
                misc(a.q(), 0, 1, 0b10110)
            }
            _ => return Ok(None),
        };
        // `fcvtl2` reads the upper half and `fcvtn2` writes it, and a name without the `2` asks
        // for the lower one.
        let half = if base == "fcvtl" { b.q() } else { a.q() };
        if half != second {
            return Err(self.register());
        }
        Ok(Some(word))
    }

    /// The shifts by a number, including the ones that widen or narrow the lanes, and the two
    /// names that are a widening shift by nothing.
    fn shifts(self, values: &[Value]) -> Result<Option<u32>, Error> {
        let m = self.mnemonic;
        let shift = |q: u32, u: u32, immhb: u32, opcode: u32, n: u8, d: u8| {
            0x0F00_0400
                | q << 30
                | u << 29
                | immhb << 16
                | opcode << 11
                | u32::from(n) << 5
                | u32::from(d)
        };
        // A shift left carries the lane width plus the amount and a shift right twice the width
        // less it, so that the highest set bit says the width either way.
        let amount = |esize: u32, amount: i64, left: bool| -> Result<u32, Error> {
            let fits = if left {
                (0..i64::from(esize)).contains(&amount)
            } else {
                (1..=i64::from(esize)).contains(&amount)
            };
            if !fits {
                return Err(self.immediate(amount));
            }
            Ok(if left { esize + amount as u32 } else { 2 * esize - amount as u32 })
        };
        if let Some((u, opcode, left)) =
            row(&SHIFTS.map(|(name, u, opcode, left)| (name, (u, opcode, left))), m)
        {
            match values {
                [Value::Vector(a, d), Value::Vector(b, n), Value::Imm(by)] => {
                    let a = self.alike(&[*a, *b], ALL)?;
                    let immhb = amount(8 << a.size(), *by, left)?;
                    return Ok(Some(shift(a.q(), u, immhb, opcode, *n, *d)));
                }
                [Value::Fp(Scalar::D, d), Value::Fp(Scalar::D, n), Value::Imm(by)] => {
                    let immhb = amount(64, *by, left)?;
                    return Ok(Some(shift(1, u, immhb, opcode, *n, *d) | 0x1000_0000));
                }
                _ => {}
            }
        }
        if let Some((u, opcode)) = row(&FIXED.map(|(name, u, opcode)| (name, (u, opcode))), m) {
            match values {
                [Value::Vector(a, d), Value::Vector(b, n), Value::Imm(by)] => {
                    let a = self.alike(&[*a, *b], S | D)?;
                    let immhb = amount(8 << a.size(), *by, false)?;
                    return Ok(Some(shift(a.q(), u, immhb, opcode, *n, *d)));
                }
                [Value::Fp(a, d), Value::Fp(b, n), Value::Imm(by)] => {
                    let double = precision(&[*a, *b]).ok_or_else(|| self.register())?;
                    let immhb = amount(32 << double, *by, false)?;
                    return Ok(Some(shift(1, u, immhb, opcode, *n, *d) | 0x1000_0000));
                }
                _ => {}
            }
        }
        let (base, second) = upper(m);
        if let Some((u, opcode)) =
            row(&SHIFT_NARROW.map(|(name, u, opcode)| (name, (u, opcode))), base)
        {
            let [Value::Vector(a, d), Value::Vector(b, n), Value::Imm(by)] = values else {
                return Ok(None);
            };
            if a.q() != second || a.wide() != Some(*b) {
                return Err(self.register());
            }
            let immhb = amount(8 << a.size(), *by, false)?;
            return Ok(Some(shift(a.q(), u, immhb, opcode, *n, *d)));
        }
        let (u, by) = match (base, values) {
            ("sshll", [_, _, Value::Imm(by)]) => (0, *by),
            ("ushll", [_, _, Value::Imm(by)]) => (1, *by),
            ("sxtl", [_, _]) => (0, 0),
            ("uxtl", [_, _]) => (1, 0),
            _ => return Ok(None),
        };
        let [Value::Vector(a, d), Value::Vector(b, n), ..] = values else {
            return Ok(None);
        };
        if b.q() != second || b.wide() != Some(*a) {
            return Err(self.register());
        }
        let immhb = amount(8 << b.size(), by, true)?;
        Ok(Some(shift(b.q(), u, immhb, 0b10100, *n, *d)))
    }

    /// Three registers of different widths.
    fn three_different(self, values: &[Value]) -> Result<Option<u32>, Error> {
        let (base, second) = upper(self.mnemonic);
        let Some((u, opcode, shape)) =
            row(&THREE_DIFFERENT.map(|(name, u, opcode, shape)| (name, (u, opcode, shape))), base)
        else {
            return Ok(None);
        };
        let [Value::Vector(a, d), Value::Vector(b, n), Value::Vector(c, r)] = values else {
            return Ok(None);
        };
        // The narrow operand, and the two that have to be the wide one.
        let (narrow, wide) = match shape {
            Shape::Long => {
                if b != c {
                    return Err(self.register());
                }
                (*b, [*a, *a])
            }
            Shape::Wide => (*c, [*a, *b]),
            Shape::Narrow => (*a, [*b, *c]),
        };
        if narrow.q() != second || wide.iter().any(|&wide| narrow.wide() != Some(wide)) {
            return Err(self.register());
        }
        // The saturating doubling ones have no byte form, and the polynomial one only has that.
        let sizes = match base {
            "pmull" => B,
            "sqdmlal" | "sqdmlsl" | "sqdmull" => H | S,
            _ => BHS,
        };
        if !narrow.takes(sizes) {
            return Err(self.register());
        }
        Ok(Some(
            0x0E20_0000
                | narrow.q() << 30
                | u << 29
                | narrow.size() << 22
                | u32::from(*r) << 16
                | opcode << 12
                | u32::from(*n) << 5
                | u32::from(*d),
        ))
    }

    /// The permutes, `ext`, and the table lookups.
    fn permute(self, values: &[Value]) -> Result<Option<u32>, Error> {
        let m = self.mnemonic;
        let word = match (m, values) {
            (_, [Value::Vector(a, d), Value::Vector(b, n), Value::Vector(c, r)])
                if row(&PERMUTE, m).is_some() =>
            {
                let a = self.alike(&[*a, *b, *c], ALL)?;
                0x0E00_0800
                    | a.q() << 30
                    | a.size() << 22
                    | u32::from(*r) << 16
                    | row(&PERMUTE, m).unwrap_or(0) << 12
                    | u32::from(*n) << 5
                    | u32::from(*d)
            }
            (
                "ext",
                [Value::Vector(a, d), Value::Vector(b, n), Value::Vector(c, r), Value::Imm(at)],
            ) => {
                let a = self.alike(&[*a, *b, *c], B)?;
                let at = self.number(*at, 8 << a.q())?;
                0x2E00_0000
                    | a.q() << 30
                    | u32::from(*r) << 16
                    | at << 11
                    | u32::from(*n) << 5
                    | u32::from(*d)
            }
            (
                "tbl" | "tbx",
                [Value::Vector(a, d), Value::List(Arrangement::B16, n, count), Value::Vector(b, r)],
            ) => {
                let a = self.alike(&[*a, *b], B)?;
                if !(1..=4).contains(count) {
                    return Err(self.register());
                }
                0x0E00_0000
                    | a.q() << 30
                    | u32::from(*r) << 16
                    | u32::from(count - 1) << 13
                    | u32::from(m == "tbx") << 12
                    | u32::from(*n) << 5
                    | u32::from(*d)
            }
            _ => return Ok(None),
        };
        Ok(Some(word))
    }

    /// The reductions of one register into a scalar.
    fn reduce(self, values: &[Value]) -> Result<Option<u32>, Error> {
        let m = self.mnemonic;
        let [Value::Fp(s, d), Value::Vector(a, n)] = values else {
            return Ok(None);
        };
        let (d, n) = (u32::from(*d), u32::from(*n));
        if let Some((u, opcode, long)) =
            row(&ACROSS.map(|(name, u, opcode, long)| (name, (u, opcode, long))), m)
        {
            let lane = s.size().map(|size| size - u32::from(long));
            let lanes = matches!(
                a,
                Arrangement::B8
                    | Arrangement::B16
                    | Arrangement::H4
                    | Arrangement::H8
                    | Arrangement::S4
            );
            if !lanes || lane != Some(a.size()) {
                return Err(self.register());
            }
            return Ok(Some(
                0x0E30_0800 | a.q() << 30 | u << 29 | a.size() << 22 | opcode << 12 | n << 5 | d,
            ));
        }
        if let Some((high, opcode)) =
            row(&FP_ACROSS.map(|(name, high, opcode)| (name, (high, opcode))), m)
        {
            if (*s, *a) != (Scalar::S, Arrangement::S4) {
                return Err(self.register());
            }
            return Ok(Some(0x6E30_0800 | high << 23 | opcode << 12 | n << 5 | d));
        }
        if let Some((high, opcode)) =
            row(&FP_PAIR.map(|(name, high, opcode)| (name, (high, opcode))), m)
        {
            let double = match (s, a) {
                (Scalar::S, Arrangement::S2) => 0,
                (Scalar::D, Arrangement::D2) => 1,
                _ => return Err(self.register()),
            };
            return Ok(Some(0x7E30_0800 | high << 23 | double << 22 | opcode << 12 | n << 5 | d));
        }
        if m == "addp" && (*s, *a) == (Scalar::D, Arrangement::D2) {
            return Ok(Some(0x5EF1_B800 | n << 5 | d));
        }
        Ok(None)
    }

    /// The multiplies by one element of a register.
    fn element(self, values: &[Value]) -> Result<Option<u32>, Error> {
        let m = self.mnemonic;
        let (base, second) = upper(m);
        let place = |size: u32, r: u8, i: u8| by_element(size, r, i).ok_or_else(|| self.register());
        let esize = |scalar: Scalar| scalar.size().ok_or_else(|| self.register());
        if let Some((u, opcode)) =
            row(&FP_BY_ELEMENT.map(|(name, u, opcode)| (name, (u, opcode))), m)
        {
            let word = match values {
                [Value::Vector(a, d), Value::Vector(b, n), Value::Element(e, r, i)] => {
                    let a = self.alike(&[*a, *b], S | D)?;
                    if esize(*e)? != a.size() {
                        return Err(self.register());
                    }
                    0x0F80_0000
                        | a.q() << 30
                        | u << 29
                        | u32::from(a.size() == 3) << 22
                        | place(a.size(), *r, *i)?
                        | opcode << 12
                        | u32::from(*n) << 5
                        | u32::from(*d)
                }
                [Value::Fp(s, d), Value::Fp(t, n), Value::Element(e, r, i)] => {
                    let size = esize(*s)?;
                    if s != t || s != e || size < 2 {
                        return Err(self.register());
                    }
                    0x5F80_0000
                        | u << 29
                        | u32::from(size == 3) << 22
                        | place(size, *r, *i)?
                        | opcode << 12
                        | u32::from(*n) << 5
                        | u32::from(*d)
                }
                _ => return Ok(None),
            };
            return Ok(Some(word));
        }
        let [Value::Vector(a, d), Value::Vector(b, n), Value::Element(e, r, i)] = values else {
            return Ok(None);
        };
        let (u, opcode, narrow) = if let Some((u, opcode)) =
            row(&BY_ELEMENT.map(|(name, u, opcode)| (name, (u, opcode))), m)
        {
            (u, opcode, self.alike(&[*a, *b], H | S)?)
        } else if let Some((u, opcode)) =
            row(&BY_ELEMENT_LONG.map(|(name, u, opcode)| (name, (u, opcode))), base)
        {
            if b.q() != second || b.wide() != Some(*a) || !b.takes(H | S) {
                return Err(self.register());
            }
            (u, opcode, *b)
        } else {
            return Ok(None);
        };
        if esize(*e)? != narrow.size() {
            return Err(self.register());
        }
        Ok(Some(
            0x0F00_0000
                | narrow.q() << 30
                | u << 29
                | narrow.size() << 22
                | place(narrow.size(), *r, *i)?
                | opcode << 12
                | u32::from(*n) << 5
                | u32::from(*d),
        ))
    }

    /// The loads and stores of whole registers named as a list, `ld1 {v0.16b}, [x0]` and its
    /// siblings that spread the lanes over two, three or four registers, and the loads of one
    /// element into every lane.
    fn structures(self, values: &[Value]) -> Result<Option<u32>, Error> {
        let m = self.mnemonic;
        let (list, rest) = match values {
            [Value::List(a, t, count), rest @ ..] => ((*a, *t, *count), rest),
            _ => return Ok(None),
        };
        let (a, t, count) = list;
        let load = u32::from(m.starts_with("ld"));
        // The word, and how many bytes a post-indexed form moves the base by.
        let (word, bytes) = match m {
            "ld1" | "st1" | "ld2" | "st2" | "ld3" | "st3" | "ld4" | "st4" => {
                let opcode = match (&m[2..], count) {
                    ("1", 1) => 0b0111,
                    ("1", 2) => 0b1010,
                    ("1", 3) => 0b0110,
                    ("1", 4) => 0b0010,
                    ("2", 2) => 0b1000,
                    ("3", 3) => 0b0100,
                    ("4", 4) => 0b0000,
                    _ => return Err(self.register()),
                };
                if a == Arrangement::D1 && count != 1 && &m[2..] != "1" {
                    return Err(self.register());
                }
                let word = 0x0C00_0000 | a.q() << 30 | load << 22 | opcode << 12 | a.size() << 10;
                (word, u32::from(count) * (8 << a.q()))
            }
            "ld1r" | "ld2r" | "ld3r" | "ld4r" => {
                let wanted = u32::from(m.as_bytes()[2] - b'0');
                if u32::from(count) != wanted {
                    return Err(self.register());
                }
                let (three, pair) = ((wanted - 1) >> 1, (wanted - 1) & 1);
                let word = 0x0D40_C000 | a.q() << 30 | pair << 21 | three << 13 | a.size() << 10;
                (word, wanted << a.size())
            }
            _ => return Ok(None),
        };
        let word = word | u32::from(t);
        let base = |addr: &Addr| u32::from(addr.base) << 5;
        // A post-indexed form is bit twenty three and a register in the field where the plain one
        // has zero, where thirty one means the base moves by the size of what was accessed.
        let post = 0x0080_0000;
        Ok(Some(match rest {
            [Value::Mem(addr @ Addr { offset: Offset::Imm(0), mode: Mode::Offset, .. })] => {
                word | base(addr)
            }
            [Value::Mem(addr @ Addr { offset: Offset::Imm(by), mode: Mode::Post, .. })] => {
                if *by != i64::from(bytes) {
                    return Err(self.immediate(*by));
                }
                word | post | 31 << 16 | base(addr)
            }
            [
                Value::Mem(addr @ Addr { offset: Offset::Imm(0), mode: Mode::Offset, .. }),
                Value::Gpr(Width::X, r),
            ] if *r < 31 => word | post | u32::from(*r) << 16 | base(addr),
            _ => return Err(self.unwritten()),
        }))
    }
}

/// The word for a modified immediate: `Q`, `op`, the placing field and the eight bits.
fn modified(q: u32, op: u32, cmode: u32, imm8: u32, d: u8) -> u32 {
    0x0F00_0400
        | q << 30
        | op << 29
        | (imm8 >> 5) << 16
        | cmode << 12
        | (imm8 & 31) << 5
        | u32::from(d)
}

#[cfg(test)]
mod tests {
    use super::super::encode;
    use crate::aarch64::read;

    /// The word for a line, or why there is none.
    fn word(text: &str) -> Result<u32, String> {
        let line = read(text).map_err(|e| e.to_string())?;
        let encoded = encode(&line.mnemonic, &line.values).map_err(|e| e.to_string())?;
        match encoded.fixup {
            None => Ok(encoded.word),
            Some(_) => Err("a fixup".to_owned()),
        }
    }

    #[test]
    fn every_vector_word_gnu_as_wrote_is_the_word_written_here() {
        let mut wrong = Vec::new();
        let mut count = 0;
        for line in include_str!("neon.txt").lines() {
            if line.starts_with('#') || line.is_empty() {
                continue;
            }
            let (want, text) = line.split_once('\t').expect("a word and a line");
            let want = u32::from_str_radix(want, 16).expect("a word in hex");
            count += 1;
            match word(text) {
                Ok(got) if got == want => {}
                Ok(got) => wrong.push(format!("{text}: wrote {got:08x}, GNU as wrote {want:08x}")),
                Err(e) => wrong.push(format!("{text}: {e}")),
            }
        }
        assert!(count > 1500, "neon.txt has only {count} lines");
        assert!(wrong.is_empty(), "{} of {count} lines differ:\n{}", wrong.len(), wrong.join("\n"));
    }

    #[test]
    fn every_vector_line_gnu_as_refuses_is_refused_here() {
        let mut taken = Vec::new();
        for text in include_str!("neon-refused.txt").lines() {
            if text.starts_with('#') || text.is_empty() {
                continue;
            }
            if let Ok(got) = word(text) {
                taken.push(format!("{text}: wrote {got:08x}"));
            }
        }
        assert!(taken.is_empty(), "{} lines were taken:\n{}", taken.len(), taken.join("\n"));
    }
}
