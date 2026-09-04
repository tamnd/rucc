//! Every instruction this target writes, as bytes and as text, one per line.
//!
//! The input to the differential disassembly check `spec/11-asm-objects-debug.md` section 11.1
//! asks for, which is `cargo xtask disasm`. Each line is the bytes we encode an instruction to,
//! then a bar, then the assembly we print for the same instruction. The check reads an
//! independent decoder's account of each half and holds the two accounts to being the same
//! instruction.
//!
//! The listing is every instruction in the table crossed with enough operands to reach the cases
//! the encoding turns on: a register the machine had from the start and one it gained later, an
//! address of every shape, and an immediate of every width. Instructions naming a symbol or a
//! label are left out, because what they encode to is not settled until something says where the
//! symbol went.

use rucc_target::x86_64::{
    Addr, Arg, GPR, INSTS, R8, R9, R10, R11, R12, R13, RAX, RBP, RCX, RDX, RSI, RSP, Value, Width,
    encode, gpr_name, written,
};
use rucc_target::{Constraint, PhysReg, RegClass};

/// What we call a register in the assembly we print, which the decoder has to agree with.
///
/// The class is the operand's rather than a guess from the mnemonic, so an instruction that names
/// one register from each file is written correctly and a new vector instruction needs nothing
/// added here.
fn name(reg: PhysReg, width: Width, class: RegClass) -> String {
    if class == GPR {
        format!("%{}", gpr_name(reg, width).expect("every width of a general register has a name"))
    } else {
        format!("%xmm{}", reg.number())
    }
}

/// One address of every shape the encoding treats differently.
///
/// The stack pointer and the frame pointer are in here twice over, once as themselves and once as
/// the two registers the machine gained later that are written the same way, because those four
/// are the cases an address cannot be written plainly in.
fn addresses() -> Vec<(Addr, String)> {
    let at = |base, index, scale, disp| Addr { base, index, scale, disp, rip: false };
    vec![
        (at(Some(RCX), None, 0, 0), "(%rcx)".to_owned()),
        (at(Some(RCX), None, 0, -16), "-16(%rcx)".to_owned()),
        (at(Some(RCX), None, 0, 1000), "1000(%rcx)".to_owned()),
        (at(Some(RSP), None, 0, 8), "8(%rsp)".to_owned()),
        (at(Some(RBP), None, 0, 0), "0(%rbp)".to_owned()),
        (at(Some(R12), None, 0, 8), "8(%r12)".to_owned()),
        (at(Some(R13), None, 0, 0), "0(%r13)".to_owned()),
        (at(Some(RCX), Some(RDX), 4, -16), "-16(%rcx,%rdx,4)".to_owned()),
        (at(Some(R8), Some(R9), 8, 0), "(%r8,%r9,8)".to_owned()),
        (at(None, Some(RDX), 2, 32), "32(,%rdx,2)".to_owned()),
        (at(None, None, 0, 64), "64".to_owned()),
    ]
}

fn main() {
    let banks = [[RAX, RCX, RDX, RSI], [R8, R9, R10, R11]];
    let immediates: [i64; 4] = [1, -1, 1000, 0x1_2345_6789];
    let mut lines = Vec::new();

    for &(opcode, form) in INSTS {
        let operands = form.operands();
        for inst in written(opcode).expect("every opcode in the table is written") {
            if inst.args.iter().any(|arg| matches!(arg, Arg::Symbol | Arg::Label)) {
                continue;
            }
            let has = |kind: fn(&Arg) -> bool| inst.args.iter().any(kind);
            let mems = if has(|arg| matches!(arg, Arg::Mem)) {
                addresses()
            } else {
                vec![(Addr::default(), String::new())]
            };
            let imms =
                if has(|arg| matches!(arg, Arg::Imm)) { immediates.to_vec() } else { vec![0] };

            for bank in banks {
                for (addr, addr_text) in &mems {
                    for &imm in &imms {
                        let mut values = Vec::new();
                        let mut text = Vec::new();
                        let mut high = false;
                        for arg in inst.args {
                            match *arg {
                                Arg::Reg(at, width) => {
                                    // An operand pinned to a register is that register and
                                    // nothing else, which is what makes every shift count %cl.
                                    let desc = operands[usize::from(at)];
                                    let reg = match desc.constraint {
                                        Constraint::Fixed(fixed) => fixed,
                                        _ => bank[usize::from(at) % bank.len()],
                                    };
                                    values.push(Value::Reg(reg, width));
                                    text.push(name(reg, width, desc.class));
                                }
                                // A call names no operand in the table, so there is no constraint
                                // to read and any register at all is one it could go through.
                                Arg::Through => {
                                    let reg = bank[0];
                                    values.push(Value::Reg(reg, Width::Quad));
                                    text.push(format!("*{}", name(reg, Width::Quad, GPR)));
                                }
                                Arg::Named(named) => {
                                    high = true;
                                    values.push(Value::High(RAX));
                                    text.push(format!("%{named}"));
                                }
                                Arg::Imm => {
                                    values.push(Value::Imm(imm));
                                    text.push(format!("${imm}"));
                                }
                                Arg::Mem => {
                                    values.push(Value::Mem(*addr));
                                    text.push(addr_text.clone());
                                }
                                Arg::Symbol | Arg::Label => unreachable!("filtered above"),
                            }
                        }
                        // The high half of a register cannot share an instruction with one of the
                        // registers the machine gained later, so the second bank has nothing to
                        // say about an instruction naming it.
                        if high && bank[0] != RAX {
                            continue;
                        }
                        let mut bytes = Vec::new();
                        match encode(inst.mnemonic, &values, &mut bytes) {
                            Ok(_) => {}
                            Err(e) => {
                                eprintln!("{}: {e}", inst.mnemonic);
                                continue;
                            }
                        }
                        let hex: Vec<String> =
                            bytes.iter().map(|byte| format!("{byte:02x}")).collect();
                        let written = match text.is_empty() {
                            true => inst.mnemonic.to_owned(),
                            false => format!("{} {}", inst.mnemonic, text.join(", ")),
                        };
                        lines.push(format!("{}|{written}", hex.join(" ")));
                    }
                }
            }
        }
    }

    println!("{}", lines.join("\n"));
    eprintln!("{} instructions", lines.len());
}
