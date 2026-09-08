//! The described classifier against the cases the compiler's hand written ones are tested on.
//!
//! `spec/cross-compile/06-abis.md` section 6.7 proposes replacing four hand written classifiers with one
//! classifier and four descriptions, and the only evidence that matters for that proposal is
//! that the replacement answers the same thing. So these are the compiler's own cases, ported,
//! including the ones that exist because somebody got them wrong once.
//!
//! Two of them are worth reading even if the rest are skipped. `an_aggregate_that_did_not_fit`
//! and `sysv_lets_a_later_argument_have_the_register` are the same situation on two ABIs with
//! opposite answers, and that opposition is the reason [`rucc_abi::Short`] is a field rather
//! than a constant.

use rucc_abi::abis::{AAPCS64, DARWIN_ARM64, RISCV_LP64D, SYSV_AMD64, WIN64};
use rucc_abi::{Arg, Call, Format, Kind, Pass, Piece, Scalar, Shape, Slot, pieces, record};

/// An integer of this size, aligned to itself.
fn int(size: u64) -> Scalar {
    Scalar::integer(size)
}

/// A floating point value in this format, of this size, aligned to itself.
fn float(format: Format, size: u64) -> Scalar {
    Scalar::float(format, size)
}

/// A general purpose register holding this many bytes from this offset.
const fn gpr(offset: u64, size: u32) -> Slot {
    Slot::Integer { offset, size }
}

/// A vector register holding this format from this offset.
const fn fpr(offset: u64, format: Format) -> Slot {
    Slot::Float { offset, format }
}

/// A call on x86-64 Linux with nothing spent yet.
fn sysv() -> Call {
    SYSV_AMD64.call()
}

/// A call on AArch64 Linux with nothing spent yet.
fn aapcs() -> Call {
    AAPCS64.call()
}

/// A call on Windows x64 with nothing spent yet.
fn win64() -> Call {
    WIN64.call()
}

/// A call on RISC-V Linux with nothing spent yet.
fn riscv() -> Call {
    RISCV_LP64D.call()
}

#[test]
fn sysv_puts_two_integers_in_two_registers() {
    let members = pieces(&[int(4), int(4)]);
    let shape = record(&members);
    assert_eq!(shape.size, 8);
    assert_eq!(sysv().argument(&Arg::Aggregate(shape)), Pass::Pieces(vec![gpr(0, 8)]));

    let members = pieces(&[int(4), int(4), int(4)]);
    let shape = record(&members);
    assert_eq!(shape.size, 12);
    // The second register holds the four bytes that are left rather than eight that are not
    // there, because the object stops at twelve and a wider load can fault.
    assert_eq!(sysv().argument(&Arg::Aggregate(shape)), Pass::Pieces(vec![gpr(0, 8), gpr(8, 4)]));
}

#[test]
fn sysv_sends_a_float_beside_an_int_into_a_general_register() {
    // The classic one. `struct { int a; float b; }` is one eightbyte holding both, the merge
    // rule says INTEGER, and the float arrives in the low half of a general purpose register
    // rather than in a vector one.
    let members = pieces(&[int(4), float(Format::Single, 4)]);
    assert_eq!(sysv().argument(&Arg::Aggregate(record(&members))), Pass::Pieces(vec![gpr(0, 8)]));

    // Eight bytes apart they are in different eightbytes and each goes where it belongs.
    let members = pieces(&[int(8), float(Format::Double, 8)]);
    assert_eq!(
        sysv().argument(&Arg::Aggregate(record(&members))),
        Pass::Pieces(vec![gpr(0, 8), fpr(8, Format::Double)])
    );
}

#[test]
fn sysv_reads_two_floats_in_one_eightbyte_as_one_double() {
    let members = pieces(&[float(Format::Single, 4), float(Format::Single, 4)]);
    assert_eq!(
        sysv().argument(&Arg::Aggregate(record(&members))),
        Pass::Pieces(vec![fpr(0, Format::Double)])
    );

    // One `float` on its own is four bytes, and reading eight of them would read past the
    // object, so the slot is as wide as the object is.
    let members = pieces(&[float(Format::Single, 4)]);
    assert_eq!(
        sysv().argument(&Arg::Aggregate(record(&members))),
        Pass::Pieces(vec![fpr(0, Format::Single)])
    );
}

#[test]
fn sysv_puts_anything_over_sixteen_bytes_in_memory() {
    let members = pieces(&[int(8), int(8), int(8)]);
    assert_eq!(sysv().argument(&Arg::Aggregate(record(&members))), Pass::Memory);
    // And brings it back through a hidden pointer rather than through the argument area.
    assert_eq!(sysv().returns(&Arg::Aggregate(record(&members))), Pass::Reference);
}

#[test]
fn sysv_sends_a_misaligned_member_to_memory_and_a_bitfield_not() {
    // `struct __attribute__((packed)) { char c; int b; }`, where `b` sits at offset one.
    let members = [
        Piece { offset: 0, scalar: int(1) },
        Piece { offset: 1, scalar: Scalar { kind: Kind::Integer, size: 4, align: 4 } },
    ];
    let shape = Shape { size: 5, align: 1, pieces: &members, complex: false };
    assert_eq!(sysv().argument(&Arg::Aggregate(shape)), Pass::Memory);

    // A bit-field may sit anywhere, which is what an alignment of one says, and it sends
    // nothing to memory.
    let members = [
        Piece { offset: 0, scalar: int(1) },
        Piece { offset: 1, scalar: Scalar { kind: Kind::Integer, size: 4, align: 1 } },
    ];
    let shape = Shape { size: 5, align: 1, pieces: &members, complex: false };
    assert_eq!(sysv().argument(&Arg::Aggregate(shape)), Pass::Pieces(vec![gpr(0, 5)]));
}

#[test]
fn sysv_takes_a_long_double_to_memory_going_in_and_the_x87_stack_coming_back() {
    let members = pieces(&[float(Format::X87Extended, 16)]);
    let shape = record(&members);
    assert_eq!(shape.size, 16);
    assert_eq!(sysv().argument(&Arg::Aggregate(shape)), Pass::Memory);
    assert_eq!(
        sysv().returns(&Arg::Aggregate(shape)),
        Pass::Pieces(vec![fpr(0, Format::X87Extended)])
    );
}

#[test]
fn sysv_brings_back_a_complex_long_double_where_a_record_of_two_goes_to_memory() {
    let members = pieces(&[float(Format::X87Extended, 16), float(Format::X87Extended, 16)]);
    let complex = Shape { complex: true, ..record(&members) };
    assert_eq!(
        sysv().returns(&Arg::Aggregate(complex)),
        // Two registers of the x87 stack holding the real part and then the imaginary one,
        // sixteen bytes apart because that is where the members are.
        Pass::Pieces(vec![fpr(0, Format::X87Extended), fpr(16, Format::X87Extended)])
    );
    // The same thirty two bytes with the same two members in the same places, and the only
    // thing that tells them apart is the flag.
    assert_eq!(sysv().returns(&Arg::Aggregate(record(&members))), Pass::Reference);
}

#[test]
fn sysv_lets_a_later_argument_have_the_register() {
    let members = pieces(&[int(8), int(8)]);
    let shape = Arg::Aggregate(record(&members));
    let mut call = sysv();
    // Five integer arguments leave one register, and this wants two.
    for _ in 0..5 {
        assert_eq!(call.argument(&Arg::Scalar(int(4))), Pass::Direct);
    }
    assert_eq!(call.argument(&shape), Pass::Memory);
    // A scalar after it still travels as itself in the register that is left. This is the
    // opposite of what AAPCS64 does with the same situation, and it is why running short is a
    // field of the rule rather than one behaviour the classifier hard codes.
    assert_eq!(call.integer_left(), 1);
    assert_eq!(call.argument(&Arg::Scalar(int(4))), Pass::Direct);
}

#[test]
fn sysv_spends_an_argument_register_on_the_returned_pointer() {
    let big = pieces(&[int(8), int(8), int(8)]);
    let pair = pieces(&[int(8), int(8)]);
    let mut call = sysv();
    assert_eq!(call.returns(&Arg::Aggregate(record(&big))), Pass::Reference);
    // Five registers left rather than six, so a two register aggregate fits after three
    // integers and would not have fitted after five.
    assert_eq!(call.integer_left(), 5);
    for _ in 0..3 {
        assert_eq!(call.argument(&Arg::Scalar(int(4))), Pass::Direct);
    }
    assert_eq!(
        call.argument(&Arg::Aggregate(record(&pair))),
        Pass::Pieces(vec![gpr(0, 8), gpr(8, 8)])
    );
}

#[test]
fn sysv_gives_an_int128_two_registers_and_a_long_double_none() {
    let pair = pieces(&[int(8), int(8)]);
    let mut call = sysv();
    for _ in 0..2 {
        assert_eq!(call.argument(&Arg::Scalar(int(16))), Pass::Direct);
    }
    // Four of the six are gone, so the aggregate fits, and the `long double` between them spent
    // nothing on its way past.
    assert_eq!(call.argument(&Arg::Scalar(float(Format::X87Extended, 16))), Pass::Direct);
    assert_eq!(call.integer_left(), 2);
    assert_eq!(
        call.argument(&Arg::Aggregate(record(&pair))),
        Pass::Pieces(vec![gpr(0, 8), gpr(8, 8)])
    );
}

#[test]
fn sysv_gives_an_int128_no_register_rather_than_half_of_one() {
    let pair = pieces(&[int(8), int(8)]);
    let mut call = sysv();
    // Five of the six, so an `__int128` cannot have the two it needs. Spending the sixth on
    // half of it would spend it on nothing, and the structure after it would go to memory with
    // a register still free.
    for _ in 0..5 {
        assert_eq!(call.argument(&Arg::Scalar(int(8))), Pass::Direct);
    }
    assert_eq!(call.argument(&Arg::Scalar(int(16))), Pass::Direct);
    assert_eq!(call.integer_left(), 1);
    assert_eq!(call.argument(&Arg::Scalar(int(8))), Pass::Direct);
    // Now it really is spent, so the pair of eight byte fields has nowhere to go.
    assert_eq!(call.argument(&Arg::Aggregate(record(&pair))), Pass::Memory);
}

#[test]
fn an_aggregate_of_no_size_travels_nowhere_on_every_abi() {
    let shape = Shape { size: 0, align: 1, pieces: &[], complex: false };
    for mut call in [sysv(), aapcs(), win64(), riscv(), DARWIN_ARM64.call()] {
        assert_eq!(call.argument(&Arg::Aggregate(shape)), Pass::Ignore);
        assert_eq!(call.returns(&Arg::Aggregate(shape)), Pass::Ignore);
        assert_eq!(call.returns(&Arg::Void), Pass::Ignore);
    }
}

#[test]
fn aapcs_puts_three_floats_in_three_vector_registers_and_one_int_ends_that() {
    let members = pieces(&[float(Format::Single, 4); 3]);
    assert_eq!(
        aapcs().argument(&Arg::Aggregate(record(&members))),
        Pass::Pieces(vec![fpr(0, Format::Single), fpr(4, Format::Single), fpr(8, Format::Single)])
    );

    // `struct { float a, b, c; int d; }` is sixteen bytes of mixed members, which is two
    // general purpose registers and no longer homogeneous.
    let members = pieces(&[
        float(Format::Single, 4),
        float(Format::Single, 4),
        float(Format::Single, 4),
        int(4),
    ]);
    assert_eq!(
        aapcs().argument(&Arg::Aggregate(record(&members))),
        Pass::Pieces(vec![gpr(0, 8), gpr(8, 8)])
    );
}

#[test]
fn aapcs_stops_being_homogeneous_at_five_members() {
    let members = pieces(&[float(Format::Single, 4); 5]);
    let shape = record(&members);
    assert_eq!(shape.size, 20);
    // Twenty bytes, which is over the sixteen an aggregate can travel in registers in.
    assert_eq!(aapcs().argument(&Arg::Aggregate(shape)), Pass::Reference);
}

#[test]
fn aapcs_does_not_call_a_float_with_padding_after_it_homogeneous() {
    // `struct { float a; char pad[12]; }` has one floating point member and is not an HFA,
    // because the members do not fill it.
    let mut members = vec![Piece { offset: 0, scalar: float(Format::Single, 4) }];
    for at in 4..16 {
        members.push(Piece { offset: at, scalar: int(1) });
    }
    let shape = Shape { size: 16, align: 4, pieces: &members, complex: false };
    assert_eq!(aapcs().argument(&Arg::Aggregate(shape)), Pass::Pieces(vec![gpr(0, 8), gpr(8, 8)]));
}

#[test]
fn aapcs_costs_the_arguments_nothing_for_a_large_return_value() {
    let big = pieces(&[int(8), int(8), int(8)]);
    let pair = pieces(&[int(8), int(8)]);
    let mut call = aapcs();
    assert_eq!(call.returns(&Arg::Aggregate(record(&big))), Pass::Reference);
    // Its address is in x8, so all eight argument registers are still here: six integers and
    // then a two register aggregate. This is the field that differs from SysV.
    assert_eq!(call.integer_left(), 8);
    for _ in 0..6 {
        assert_eq!(call.argument(&Arg::Scalar(int(8))), Pass::Direct);
    }
    assert_eq!(
        call.argument(&Arg::Aggregate(record(&pair))),
        Pass::Pieces(vec![gpr(0, 8), gpr(8, 8)])
    );
}

#[test]
fn aapcs_takes_the_rest_of_the_bank_with_an_aggregate_that_did_not_fit() {
    let pair = pieces(&[int(8), int(8)]);
    let one = pieces(&[int(8)]);
    let mut call = aapcs();
    for _ in 0..7 {
        assert_eq!(call.argument(&Arg::Scalar(int(8))), Pass::Direct);
    }
    // One register is left and this wants two, so it goes in the argument area.
    assert_eq!(call.argument(&Arg::Aggregate(record(&pair))), Pass::Memory);
    // And the one that was left is not usable by anything after it either, which is the rule
    // that separates this from SysV.
    assert_eq!(call.integer_left(), 0);
    assert_eq!(call.argument(&Arg::Aggregate(record(&one))), Pass::Memory);
}

#[test]
fn aapcs_passes_over_sixteen_bytes_as_the_address_of_a_copy() {
    // The size rule, which is the whole of AAPCS64 once the homogeneous case has declined. At
    // most sixteen bytes is one or two general purpose registers and anything larger is a
    // pointer to a copy the caller made, going in and coming back, with no partial case between
    // them. Three `long`s is the smallest thing on the far side of that line.
    let mut call = aapcs();
    let pair = pieces(&[int(8); 2]);
    let triple = pieces(&[int(8); 3]);
    assert_eq!(
        call.argument(&Arg::Aggregate(record(&pair))),
        Pass::Pieces(vec![gpr(0, 8), gpr(8, 8)])
    );
    assert_eq!(call.argument(&Arg::Aggregate(record(&triple))), Pass::Reference);
    assert_eq!(aapcs().returns(&Arg::Aggregate(record(&triple))), Pass::Reference);
}

#[test]
fn aapcs_counts_the_two_banks_apart() {
    let quad = pieces(&[float(Format::Double, 8); 4]);
    let mut call = aapcs();
    for _ in 0..8 {
        assert_eq!(call.argument(&Arg::Scalar(int(8))), Pass::Direct);
    }
    // Every general purpose register is gone and all eight vector registers are still here,
    // which is two of these.
    for _ in 0..2 {
        assert_eq!(
            call.argument(&Arg::Aggregate(record(&quad))),
            Pass::Pieces(vec![
                fpr(0, Format::Double),
                fpr(8, Format::Double),
                fpr(16, Format::Double),
                fpr(24, Format::Double),
            ])
        );
    }
    assert_eq!(call.argument(&Arg::Aggregate(record(&quad))), Pass::Memory);
}

#[test]
fn darwin_classifies_a_fixed_argument_exactly_as_aapcs_does() {
    // The first of the two fields Darwin changes is about variadic arguments, so everything
    // else has to answer identically. If this ever stops being true, one of the two
    // descriptions has drifted.
    let cases = [
        pieces(&[float(Format::Double, 8), float(Format::Double, 8)]),
        pieces(&[int(8), int(8), int(8)]),
        pieces(&[float(Format::Single, 4); 3]),
        pieces(&[int(4), float(Format::Double, 8)]),
    ];
    for members in &cases {
        let shape = Arg::Aggregate(record(members));
        assert_eq!(aapcs().argument(&shape), DARWIN_ARM64.call().argument(&shape));
        assert_eq!(aapcs().returns(&shape), DARWIN_ARM64.call().returns(&shape));
    }
}

#[test]
fn darwin_puts_every_variadic_argument_in_the_argument_area() {
    let members = pieces(&[float(Format::Double, 8), float(Format::Double, 8)]);
    let shape = Arg::Aggregate(record(&members));

    // Fixed, it is two vector registers on both.
    assert_eq!(
        DARWIN_ARM64.call().argument(&shape),
        Pass::Pieces(vec![fpr(0, Format::Double), fpr(8, Format::Double)])
    );
    // Variadic, it is on the stack on Darwin with every register still free, and unchanged
    // everywhere else. This is the divergence that makes a variadic call ABI-incompatible with
    // a non-variadic one, per spec/cross-compile/06-abis.md section 6.3.
    assert_eq!(DARWIN_ARM64.call().variadic_argument(&shape), Pass::Memory);
    assert_eq!(
        AAPCS64.call().variadic_argument(&shape),
        Pass::Pieces(vec![fpr(0, Format::Double), fpr(8, Format::Double)])
    );

    // And it does not spend anything, because there is nothing to spend it on.
    let mut call = DARWIN_ARM64.call();
    assert_eq!(call.variadic_argument(&shape), Pass::Memory);
    assert_eq!(call.float_left(), 8);
    assert_eq!(call.integer_left(), 8);
}

#[test]
fn win64_reads_a_size_a_register_holds_as_an_integer_whatever_is_in_it() {
    for scalars in [vec![int(1)], vec![int(2)], vec![int(4)], vec![int(4), int(4)]] {
        let members = pieces(&scalars);
        let shape = record(&members);
        let size = u32::try_from(shape.size).expect("a small record");
        assert_eq!(win64().argument(&Arg::Aggregate(shape)), Pass::Pieces(vec![gpr(0, size)]));
    }

    // Two `float`s are eight bytes, and eight bytes go in a general purpose register here.
    let members = pieces(&[float(Format::Single, 4), float(Format::Single, 4)]);
    assert_eq!(win64().argument(&Arg::Aggregate(record(&members))), Pass::Pieces(vec![gpr(0, 8)]));
}

#[test]
fn win64_passes_any_other_size_as_an_address() {
    // Three bytes and twenty four bytes get the same answer, which is the rule that makes this
    // ABI short.
    let members = pieces(&[int(1), int(1), int(1)]);
    assert_eq!(win64().argument(&Arg::Aggregate(record(&members))), Pass::Reference);
    let members = pieces(&[int(8), int(8), int(8)]);
    assert_eq!(win64().argument(&Arg::Aggregate(record(&members))), Pass::Reference);
    assert_eq!(win64().returns(&Arg::Aggregate(record(&members))), Pass::Reference);
}

#[test]
fn win64_classifies_the_ninth_argument_the_way_it_classifies_the_first() {
    let members = pieces(&[int(4), int(4)]);
    let mut call = win64();
    for _ in 0..6 {
        assert_eq!(call.argument(&Arg::Scalar(int(8))), Pass::Direct);
    }
    // On the stack by now, and still eight bytes of value rather than an address to them.
    assert_eq!(call.argument(&Arg::Aggregate(record(&members))), Pass::Pieces(vec![gpr(0, 8)]));
}

#[test]
fn win64_spends_one_position_whichever_bank_the_value_lands_in() {
    let mut call = win64();
    assert_eq!(call.argument(&Arg::Scalar(int(4))), Pass::Direct);
    assert_eq!(call.integer_left(), 3);
    // A `double` takes xmm1 rather than xmm0, because the position is what is spent.
    assert_eq!(call.argument(&Arg::Scalar(float(Format::Double, 8))), Pass::Direct);
    assert_eq!(call.integer_left(), 2);
}

#[test]
fn riscv_puts_two_floating_point_members_in_two_floating_point_registers() {
    let members = pieces(&[float(Format::Double, 8), float(Format::Double, 8)]);
    assert_eq!(
        riscv().argument(&Arg::Aggregate(record(&members))),
        Pass::Pieces(vec![fpr(0, Format::Double), fpr(8, Format::Double)])
    );
    // The same going out, which is fa0 and fa1.
    assert_eq!(
        riscv().returns(&Arg::Aggregate(record(&members))),
        Pass::Pieces(vec![fpr(0, Format::Double), fpr(8, Format::Double)])
    );
}

#[test]
fn riscv_puts_one_of_each_in_one_of_each() {
    let members = pieces(&[float(Format::Double, 8), int(4)]);
    assert_eq!(
        riscv().argument(&Arg::Aggregate(record(&members))),
        Pass::Pieces(vec![fpr(0, Format::Double), gpr(8, 4)])
    );
    // And the other way round, because the rule is about what the members are rather than
    // about which one is written first.
    let members = pieces(&[int(4), float(Format::Double, 8)]);
    assert_eq!(
        riscv().argument(&Arg::Aggregate(record(&members))),
        // The integer is four bytes at zero and the `double` is eight bytes at eight, which is
        // the case a run of slots without offsets on it cannot describe.
        Pass::Pieces(vec![gpr(0, 4), fpr(8, Format::Double)])
    );
}

#[test]
fn riscv_stops_the_rule_at_three_members() {
    let members = pieces(&[float(Format::Single, 4); 3]);
    let shape = record(&members);
    assert_eq!(shape.size, 12);
    assert_eq!(riscv().argument(&Arg::Aggregate(shape)), Pass::Pieces(vec![gpr(0, 8), gpr(8, 4)]));
}

#[test]
fn riscv_falls_back_to_the_size_rule_when_the_float_registers_are_gone() {
    let members = pieces(&[float(Format::Double, 8), float(Format::Double, 8)]);
    let mut call = riscv();
    for _ in 0..8 {
        assert_eq!(call.argument(&Arg::Scalar(float(Format::Double, 8))), Pass::Direct);
    }
    // Every floating point register is gone, so the two members are sixteen bytes in two
    // general purpose registers instead. This is the one rule in the five descriptions that
    // declines rather than falling back to the argument area.
    assert_eq!(
        call.argument(&Arg::Aggregate(record(&members))),
        Pass::Pieces(vec![gpr(0, 8), gpr(8, 8)])
    );
}

#[test]
fn riscv_does_not_count_a_long_double_as_a_floating_point_member() {
    // Sixteen bytes of binary128, which no floating point register on LP64D holds.
    let members = pieces(&[float(Format::Quad, 16)]);
    assert_eq!(
        riscv().argument(&Arg::Aggregate(record(&members))),
        Pass::Pieces(vec![gpr(0, 8), gpr(8, 8)])
    );
    // And a scalar one travels in the two general purpose registers rather than in a vector
    // register it does not fit in.
    let mut call = riscv();
    assert_eq!(call.argument(&Arg::Scalar(float(Format::Quad, 16))), Pass::Direct);
    assert_eq!(call.integer_left(), 6);
    assert_eq!(call.float_left(), 8);
}

#[test]
fn riscv_passes_over_two_registers_as_an_address() {
    let members = pieces(&[int(8), int(8), int(8)]);
    assert_eq!(riscv().argument(&Arg::Aggregate(record(&members))), Pass::Reference);
    assert_eq!(riscv().returns(&Arg::Aggregate(record(&members))), Pass::Reference);
}
