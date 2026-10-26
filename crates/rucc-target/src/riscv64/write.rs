//! Writing one RISC-V instruction as a line of assembly GNU as reads.
//!
//! Over the same [`Value`]s the encoder takes, so the listing `-S` writes is written from the
//! operands the object writer was handed rather than from a second description of them. Registers
//! are written by their ABI names and numbers in decimal, which is what GNU as reads and what
//! objdump prints.
//!
//! Two things are written differently from how the encoder is handed them. A branch inside one
//! opcode is handed its distance as a number, and GNU as reads a bare number there as an address,
//! so it is written as a distance from `.`. And the encoder's own pseudo instructions for the
//! frame, which GNU as does not have, are written as the instructions they stand for.

use std::fmt::Write;

use crate::riscv64::encode::{Addr, FAR, Offset, Operator, Value, fits};
use crate::riscv64::{FPR_NAMES, GPR_NAMES};

/// One instruction, as text with no indentation and no newline at the end.
///
/// The symbol is the name every [`Value::Symbol`] and [`Offset::Symbol`] in the operands stands
/// for. An operand that names one when there is none is written as `?`, which nothing reads.
///
/// A pseudo instruction of the frame that stands for two instructions comes back as two lines,
/// joined by a newline and a tab, which is how the listing indents every line.
#[must_use]
pub fn write(mnemonic: &str, values: &[Value], symbol: Option<&str>) -> String {
    let far = |mnemonic: &str, rd: u8, rs: u8, imm: i64| {
        let far = GPR_NAMES[usize::from(FAR)];
        let (rd, rs) = (GPR_NAMES[usize::from(rd)], GPR_NAMES[usize::from(rs)]);
        format!("li {far}, {imm}\n\t{mnemonic} {rd}, {rs}, {far}")
    };
    match (mnemonic, values) {
        ("addi.far", &[Value::Gpr(rd), Value::Gpr(rs), Value::Imm(imm)]) if !fits(imm, 12) => {
            return far("add", rd, rs, imm);
        }
        ("subi.far", &[Value::Gpr(rd), Value::Gpr(rs), Value::Imm(imm)])
            if !imm.checked_neg().is_some_and(|near| fits(near, 12)) =>
        {
            return far("sub", rd, rs, imm);
        }
        ("addi.far", &[rd, rs, imm]) => return line("addi", &[rd, rs, imm], symbol),
        ("subi.far", &[rd, rs, Value::Imm(imm)]) => {
            return line("addi", &[rd, rs, Value::Imm(imm.wrapping_neg())], symbol);
        }
        ("lea.far", &[Value::Gpr(rd), Value::Mem(Addr { base, offset: Offset::Imm(imm) })]) => {
            if fits(imm, 12) {
                return line("addi", &[Value::Gpr(rd), Value::Gpr(base), Value::Imm(imm)], symbol);
            }
            let (rd, base) = (GPR_NAMES[usize::from(rd)], GPR_NAMES[usize::from(base)]);
            return format!("li {rd}, {imm}\n\tadd {rd}, {base}, {rd}");
        }
        _ => {}
    }
    line(mnemonic, values, symbol)
}

fn line(mnemonic: &str, values: &[Value], symbol: Option<&str>) -> String {
    let jumps = matches!(
        mnemonic,
        "beq" | "bne" | "blt" | "bge" | "bltu" | "bgeu" | "beqz" | "bnez" | "j" | "jal"
    );
    let mut line = mnemonic.to_owned();
    for (at, value) in values.iter().enumerate() {
        line.push_str(if at == 0 { " " } else { ", " });
        match *value {
            Value::Imm(distance) if jumps => {
                let _ = write!(line, ".{distance:+}");
            }
            _ => operand(&mut line, value, symbol),
        }
    }
    line
}

fn operand(line: &mut String, value: &Value, symbol: Option<&str>) {
    match *value {
        Value::Gpr(number) => line.push_str(GPR_NAMES[usize::from(number & 31)]),
        Value::Fpr(number) => line.push_str(FPR_NAMES[usize::from(number & 31)]),
        Value::Imm(imm) => {
            let _ = write!(line, "{imm}");
        }
        Value::Mem(Addr { base, offset }) => {
            match offset {
                Offset::Imm(imm) => {
                    let _ = write!(line, "{imm}");
                }
                Offset::Symbol(operator) => named(line, operator, symbol),
            }
            let _ = write!(line, "({})", GPR_NAMES[usize::from(base & 31)]);
        }
        Value::Symbol(operator) => named(line, operator, symbol),
        Value::Round(round) => line.push_str(round.name()),
        Value::Order(order) => {
            for (bit, letter) in [(8, 'i'), (4, 'o'), (2, 'r'), (1, 'w')] {
                if order & bit != 0 {
                    line.push(letter);
                }
            }
        }
    }
}

/// The symbol with the operator that picks the part of its address an instruction wants.
fn named(line: &mut String, operator: Operator, symbol: Option<&str>) {
    let symbol = symbol.unwrap_or("?");
    let _ = match operator {
        Operator::Plain => write!(line, "{symbol}"),
        Operator::Hi => write!(line, "%hi({symbol})"),
        Operator::Lo => write!(line, "%lo({symbol})"),
        Operator::TprelHi => write!(line, "%tprel_hi({symbol})"),
        Operator::TprelLo => write!(line, "%tprel_lo({symbol})"),
        Operator::TprelAdd => write!(line, "%tprel_add({symbol})"),
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::riscv64::encode::Round;

    use Value::{Fpr, Gpr, Imm, Mem, Order, Symbol};

    #[test]
    fn registers_are_written_by_their_abi_names() {
        assert_eq!(write("add", &[Gpr(10), Gpr(2), Gpr(0)], None), "add a0, sp, zero");
        assert_eq!(write("fadd.d", &[Fpr(10), Fpr(0), Fpr(31)], None), "fadd.d fa0, ft0, ft11");
        assert_eq!(write("ret", &[], None), "ret");
    }

    #[test]
    fn an_address_is_a_displacement_and_a_base() {
        let slot = Mem(Addr { base: 8, offset: Offset::Imm(-24) });
        assert_eq!(write("ld", &[Gpr(10), slot], None), "ld a0, -24(s0)");
        let low = Mem(Addr { base: 10, offset: Offset::Symbol(Operator::Lo) });
        assert_eq!(write("sw", &[Gpr(11), low], Some("n")), "sw a1, %lo(n)(a0)");
        let thread = Mem(Addr { base: 10, offset: Offset::Symbol(Operator::TprelLo) });
        assert_eq!(write("lw", &[Gpr(10), thread], Some("t")), "lw a0, %tprel_lo(t)(a0)");
    }

    #[test]
    fn a_symbol_is_written_with_the_operator_that_picks_its_part() {
        assert_eq!(write("call", &[Symbol(Operator::Plain)], Some("f")), "call f");
        assert_eq!(write("lui", &[Gpr(10), Symbol(Operator::Hi)], Some("n")), "lui a0, %hi(n)");
        let add = [Gpr(10), Gpr(10), Gpr(4), Symbol(Operator::TprelAdd)];
        assert_eq!(write("add", &add, Some("t")), "add a0, a0, tp, %tprel_add(t)");
        assert_eq!(write("tail", &[Symbol(Operator::Plain)], None), "tail ?");
    }

    #[test]
    fn a_branch_inside_an_opcode_is_a_distance_from_the_dot() {
        assert_eq!(write("bnez", &[Gpr(13), Imm(8)], None), "bnez a3, .+8");
        assert_eq!(write("j", &[Imm(-12)], None), "j .-12");
        assert_eq!(write("addi", &[Gpr(10), Gpr(10), Imm(8)], None), "addi a0, a0, 8");
    }

    #[test]
    fn rounding_and_ordering_are_written_by_name() {
        let round = [Gpr(10), Fpr(11), Value::Round(Round::Rtz)];
        assert_eq!(write("fcvt.w.d", &round, None), "fcvt.w.d a0, fa1, rtz");
        assert_eq!(write("fence", &[Order(3), Order(3)], None), "fence rw, rw");
        assert_eq!(write("fence", &[Order(15), Order(1)], None), "fence iorw, w");
    }

    #[test]
    fn the_frame_pseudo_instructions_are_written_as_what_they_stand_for() {
        let near = |imm| [Gpr(2), Gpr(2), Imm(imm)];
        assert_eq!(write("addi.far", &near(2047), None), "addi sp, sp, 2047");
        assert_eq!(write("subi.far", &near(2048), None), "addi sp, sp, -2048");
        assert_eq!(write("addi.far", &near(2048), None), "li t5, 2048\n\tadd sp, sp, t5");
        assert_eq!(write("subi.far", &near(4096), None), "li t5, 4096\n\tsub sp, sp, t5");
        let lea = |disp| [Gpr(10), Mem(Addr { base: 8, offset: Offset::Imm(disp) })];
        assert_eq!(write("lea.far", &lea(-16), None), "addi a0, s0, -16");
        assert_eq!(write("lea.far", &lea(8192), None), "li a0, 8192\n\tadd a0, s0, a0");
    }
}
