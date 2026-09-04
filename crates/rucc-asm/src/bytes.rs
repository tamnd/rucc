//! Machine functions as the bytes of a text section.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.1. The other end of [`crate::att`], and
//! deliberately the same walk: an opcode is the list of instructions the target says it is, each
//! instruction's arguments are drawn from the operands the target says they come from, and the
//! only difference is that this hands each one to the encoder instead of writing its name. That
//! is what section 11.1 means by one description rather than two, and it is why a mistake here
//! cannot be a mistake about what an instruction is. It can only be a mistake about bytes.
//!
//! # What the encoder cannot know
//!
//! Where anything outside the instruction is. A jump carries the distance to its target and the
//! target is a block that may not have been written yet, and a call carries the distance to a
//! function that is not in this file at all. The encoder leaves four bytes for each and says
//! where it left them, and this fills in the ones it can and records the ones it cannot.
//!
//! The ones it can are the jumps inside a function, since by the end of a function every block
//! has a place. They are patched here and nothing downstream ever hears about them.
//!
//! The ones it cannot are the references to a symbol, which are a relocation: an offset into the
//! section, the name of the thing wanted, and what the linker is being asked for. Choosing which
//! relocation goes with which addressing mode is this layer's job rather than the object writer's,
//! per section 11.3, because it is a fact about the instruction and not about the file format.
//!
//! # What is not decided here
//!
//! How long a jump is. Every one of them takes four bytes for its distance whether it needs them
//! or not, which is correct and larger than it has to be. Shrinking the ones that fit in a byte is
//! relaxation, an iterate-to-fixpoint pass over the whole function, and it is not written yet.
//! Nothing here would have to change for it: it would run before this and settle the lengths.
//!
//! Alignment between functions, beyond starting each one on a sixteen byte boundary, which is what
//! every x86-64 toolchain does and what the instruction fetcher is built around. The padding is
//! written as single byte nops. A longer nop is fewer instructions to decode and the padding
//! between two functions is never executed, so there is nothing to be gained by it.

use rucc_base::Interner;
use rucc_mir::{Amode, Block, Func, Inst, Operand, defs};
use rucc_target::x86_64::{self, Addr, Arg, RAX, Value, Width};
use rucc_target::{Arch, PhysReg, TargetInfo};

use rucc_object::{Extent, Reference, Reloc, Text};

use crate::Error;

/// The prefix every x86-64 opcode carries in the machine IR.
const PREFIX: &str = "x64.";

/// What a function is aligned to, and what the space in front of one is filled with.
const ALIGN: usize = 16;

/// The one byte instruction that does nothing, which is what the space in front of a function is.
const NOP: u8 = 0x90;

/// Every function, as the bytes of a text section.
///
/// # Errors
///
/// [`Error::Machine`] for an architecture nothing here encodes, and the rest for a function that
/// should not have got this far. See [`Error`].
pub fn assemble(funcs: &[Func], names: &Interner, target: &TargetInfo) -> Result<Text, Error> {
    if target.triple.arch != Arch::X86_64 {
        return Err(Error::Machine { triple: target.triple.to_string() });
    }
    let mut text = Text::default();
    for func in funcs {
        while text.bytes.len() % ALIGN != 0 {
            text.bytes.push(NOP);
        }
        let start = text.bytes.len();
        let name = names.resolve(func.name).to_owned();
        Assembler {
            names,
            func,
            name: &name,
            text: &mut text,
            blocks: Vec::new(),
            jumps: Vec::new(),
        }
        .func()?;
        let len = text.bytes.len() - start;
        text.funcs.push(Extent { name, start, len });
    }
    Ok(text)
}

/// A jump inside a function, waiting for the block it goes to to have a place.
struct Jump {
    /// Where the four bytes the distance goes in begin.
    at: usize,
    /// Where the instruction it belongs to ends, which is what the distance is counted from.
    end: usize,
    /// The block it goes to.
    to: Block,
}

/// One function being written out.
struct Assembler<'a> {
    names: &'a Interner,
    func: &'a Func,
    name: &'a str,
    text: &'a mut Text,
    /// Where each block starts, indexed by the block's own number, or [`usize::MAX`] for one that
    /// is not in the layout.
    blocks: Vec<usize>,
    jumps: Vec<Jump>,
}

impl Assembler<'_> {
    /// The blocks, and then the jumps between them once every block has a place.
    fn func(&mut self) -> Result<(), Error> {
        self.blocks = vec![usize::MAX; self.func.block_count()];
        for block in self.func.blocks() {
            self.blocks[block.index()] = self.text.bytes.len();
            for inst in self.func.insts(block) {
                self.inst(block, inst)?;
            }
        }
        for jump in std::mem::take(&mut self.jumps) {
            let to = self.blocks[jump.to.index()];
            debug_assert_ne!(to, usize::MAX, "a jump to a block that was never laid out");
            let distance = i64::try_from(to).expect("a section this size")
                - i64::try_from(jump.end).expect("a section this size");
            let distance = i32::try_from(distance)
                .map_err(|_| Error::Distance { func: self.name.to_owned(), bytes: distance })?;
            self.text.bytes[jump.at..jump.at + 4].copy_from_slice(&distance.to_le_bytes());
        }
        Ok(())
    }

    /// One instruction of the machine IR, as however many instructions of the machine it is.
    fn inst(&mut self, block: Block, inst: Inst) -> Result<(), Error> {
        let data = self.func[inst];
        let spelled = self.names.resolve(data.opcode.name());
        let opcode = spelled.strip_prefix(PREFIX).unwrap_or(spelled);
        let Some(written) = x86_64::written(opcode) else {
            return Err(Error::Opcode { func: self.name.to_owned(), opcode: spelled.to_owned() });
        };
        let operands = &self.func[data.operands];
        for machine in written {
            // What each argument turned out to be, and what the encoder has to be told about
            // afterwards for the ones that name something it cannot see.
            let mut values = Vec::with_capacity(machine.args.len());
            let mut wanted = None;
            for arg in machine.args {
                values.push(match *arg {
                    Arg::Reg(at, width) => {
                        Value::Reg(self.phys(operands[usize::from(at)], spelled)?, width)
                    }
                    // The only register named outright on this machine is the high half of the
                    // first one, which an eight bit remainder comes back in.
                    Arg::Named(_) => Value::High(RAX),
                    // The first operand read, which is where a call puts the address it goes
                    // through. Everything in front of it is a register the call writes.
                    Arg::Through => {
                        Value::Reg(self.phys(operands[defs(operands)], spelled)?, Width::Quad)
                    }
                    Arg::Imm => Value::Imm(data.imm.map_or(0, |imm| self.func[imm].0)),
                    Arg::Mem => {
                        let amode = data.mem.map(|mem| self.func[mem]);
                        let (addr, symbol) = self.addr(operands, amode.as_ref(), spelled)?;
                        if let Some(symbol) = symbol {
                            wanted = Some((symbol, Reference::Data, i64::from(addr.disp)));
                        }
                        Value::Mem(addr)
                    }
                    Arg::Symbol => {
                        let symbol =
                            data.symbol.map(|symbol| self.names.resolve(symbol).to_owned());
                        if let Some(symbol) = symbol {
                            wanted = Some((symbol, Reference::Call, 0));
                        }
                        Value::Dest
                    }
                    // Where a conditional jump goes is the first arm, because the block layout
                    // guarantees the second is the block laid out next and is fallen into.
                    Arg::Label => Value::Dest,
                });
            }

            let holes =
                x86_64::encode(machine.mnemonic, &values, &mut self.text.bytes).map_err(|why| {
                    Error::Encode {
                        func: self.name.to_owned(),
                        opcode: spelled.to_owned(),
                        why: why.to_string(),
                    }
                })?;
            let end = self.text.bytes.len();

            // A hole is either something outside the file, which is a relocation, or a block of
            // this function, which is patched once every block has a place.
            if let Some((symbol, kind, disp)) = wanted {
                let at = match kind {
                    Reference::Call => holes.dest,
                    Reference::Data => holes.rip,
                    // An address written into an image rather than reached by an instruction.
                    // Nothing above produces one, because every reference an instruction makes
                    // is a distance from where the instruction ends.
                    Reference::Address { .. } => unreachable!("an instruction wanting an address"),
                };
                let at = at.expect("an instruction naming a symbol leaves room for the distance");
                let addend = disp - i64::try_from(end - at).expect("an instruction this long");
                self.text.relocs.push(Reloc { at, symbol, kind, addend });
            } else if let Some(at) = holes.dest {
                match self.func[block].succs.first() {
                    Some(call) => self.jumps.push(Jump { at, end, to: call.block }),
                    None => debug_assert!(false, "a jump out of a block with no arms"),
                }
            }
        }
        Ok(())
    }

    /// One address, with the operands it names resolved and the symbol it names handed back.
    ///
    /// A symbol with no base and no index is reached from the instruction pointer, which is how a
    /// global is reached in position independent code and the only way this compiler reaches one.
    /// The displacement is written into the instruction and counted again in the relocation's
    /// addend, because a linker writes the whole four bytes from the addend and never reads what
    /// was there. What is in the bytes is what the instruction meant before anything was linked,
    /// which is what a person disassembling the object file would want to see.
    fn addr(
        &self,
        operands: &[Operand],
        amode: Option<&Amode>,
        opcode: &str,
    ) -> Result<(Addr, Option<String>), Error> {
        let Some(amode) = amode else {
            return Ok((Addr::default(), None));
        };
        let base = match amode.base {
            Some(at) => Some(self.phys(operands[usize::from(at)], opcode)?),
            None => None,
        };
        let index = match amode.index {
            Some(at) => Some(self.phys(operands[usize::from(at)], opcode)?),
            None => None,
        };
        let symbol = amode.symbol.map(|symbol| self.names.resolve(symbol).to_owned());
        let rip = symbol.is_some() && base.is_none() && index.is_none();
        let addr = Addr { base, index, scale: amode.scale, disp: amode.disp, rip };
        Ok((addr, if rip { symbol } else { None }))
    }

    /// The real register one operand ended up in.
    fn phys(&self, operand: Operand, opcode: &str) -> Result<PhysReg, Error> {
        operand
            .reg
            .phys()
            .ok_or_else(|| Error::Virtual { func: self.name.to_owned(), opcode: opcode.to_owned() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use rucc_base::Interner;
    use rucc_mir::{BlockCall, Mem, Opcode, Reg};
    use rucc_target::x86_64::{GPR, RAX, RCX, RDX};
    use rucc_target::{Env, Os, Triple};

    /// A linux x86-64 target, which is the one every case here is written for.
    fn target() -> TargetInfo {
        TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu))
    }

    /// One function of one block, with those instructions in it, assembled.
    fn write(build: impl FnOnce(&mut Func, &mut Interner)) -> Text {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        build(&mut func, &mut names);
        assemble(&[func], &names, &target()).expect("a function that was allocated")
    }

    /// Those bytes, as the hexadecimal a manual writes them in.
    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect::<Vec<_>>().join(" ")
    }

    /// An addition of two registers, which is the smallest instruction with operands there is.
    fn add(func: &mut Func, names: &mut Interner) {
        let block = func.create_block();
        let add = Opcode::new(names.intern("x64.add_rr_32"));
        func.build(block, add)
            .operand(Operand::write(Reg::physical(RAX), GPR))
            .operand(Operand::read(Reg::physical(RAX), GPR))
            .operand(Operand::read(Reg::physical(RCX), GPR))
            .finish();
    }

    #[test]
    fn an_instruction_is_the_bytes_the_target_says_it_is() {
        let text = write(add);
        assert_eq!(hex(&text.bytes), "01 c8");
        assert_eq!(text.funcs, [Extent { name: "f".to_owned(), start: 0, len: 2 }]);
        assert!(text.relocs.is_empty());
    }

    #[test]
    fn an_opcode_the_machine_has_no_single_instruction_for_is_all_the_ones_it_has() {
        let text = write(|func, names| {
            let block = func.create_block();
            let cmp = Opcode::new(names.intern("x64.cmp_set_l_64"));
            func.build(block, cmp)
                .operand(Operand::write(Reg::physical(RAX), GPR))
                .operand(Operand::read(Reg::physical(RCX), GPR))
                .operand(Operand::read(Reg::physical(RDX), GPR))
                .finish();
        });
        // The comparison at the width it was asked for and then the set, which is the same two
        // instructions the assembly path writes and is why one description rather than two.
        assert_eq!(hex(&text.bytes), "48 39 d1 0f 9c c0");
    }

    #[test]
    fn an_opcode_that_is_not_an_instruction_is_no_bytes_at_all() {
        let text = write(|func, names| {
            let block = func.create_block();
            let ret = Opcode::new(names.intern("x64.ret_val_32"));
            func.build(block, ret).operand(Operand::read(Reg::physical(RAX), GPR)).finish();
        });
        assert!(text.bytes.is_empty(), "{:?}", text.bytes);
    }

    #[test]
    fn a_jump_inside_a_function_is_filled_in_rather_than_left_to_the_linker() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let first = func.create_block();
        let second = func.create_block();
        let add = Opcode::new(names.intern("x64.add_rr_32"));
        func.build(first, add)
            .operand(Operand::write(Reg::physical(RAX), GPR))
            .operand(Operand::read(Reg::physical(RAX), GPR))
            .operand(Operand::read(Reg::physical(RCX), GPR))
            .finish();
        let jmp = Opcode::new(names.intern("x64.jmp"));
        func.build(second, jmp).finish();
        func.succs_mut(second).push(BlockCall::to(first));

        let text = assemble(&[func], &names, &target()).expect("two blocks");
        // Two bytes of addition, then a jump back over itself and over them, which is seven bytes
        // backwards because a jump counts from where it ends.
        assert_eq!(hex(&text.bytes), "01 c8 e9 f9 ff ff ff");
        assert!(text.relocs.is_empty(), "a jump inside a function is not the linker's business");
    }

    #[test]
    fn a_call_leaves_the_linker_the_name_of_what_it_calls() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        let call = Opcode::new(names.intern("x64.call"));
        let callee = names.intern("puts");
        func.build(block, call).symbol(callee).finish();

        let text = assemble(&[func], &names, &target()).expect("a call");
        assert_eq!(hex(&text.bytes), "e8 00 00 00 00");
        assert_eq!(
            text.relocs,
            [Reloc { at: 1, symbol: "puts".to_owned(), kind: Reference::Call, addend: -4 }]
        );
    }

    #[test]
    fn a_global_is_a_relocation_counted_from_the_end_of_the_instruction() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        let load = Opcode::new(names.intern("x64.mov_rm_64"));
        let global = names.intern("counter");
        func.build(block, load)
            .operand(Operand::write(Reg::physical(RAX), GPR))
            .mem(Mem::of(global).plus(8))
            .finish();

        let text = assemble(&[func], &names, &target()).expect("a load of a global");
        assert_eq!(hex(&text.bytes), "48 8b 05 08 00 00 00");
        // Four bytes back to where the instruction ends, and then the eight the address already
        // meant. A relocation counts from where its own bytes start and an instruction counts
        // from where it ends, and the addend is what makes up the difference.
        assert_eq!(
            text.relocs,
            [Reloc { at: 3, symbol: "counter".to_owned(), kind: Reference::Data, addend: 4 }]
        );
    }

    #[test]
    fn an_address_that_names_a_register_is_not_a_relocation() {
        let text = write(|func, names| {
            let block = func.create_block();
            let lea = Opcode::new(names.intern("x64.lea_64"));
            func.build(block, lea)
                .operand(Operand::write(Reg::physical(RAX), GPR))
                .mem(
                    Mem::at(Operand::read(Reg::physical(RCX), GPR))
                        .indexed(Operand::read(Reg::physical(RDX), GPR), 4)
                        .plus(-16),
                )
                .finish();
        });
        assert_eq!(hex(&text.bytes), "48 8d 44 91 f0");
        assert!(text.relocs.is_empty());
    }

    #[test]
    fn every_function_starts_on_a_boundary_and_the_space_in_front_of_one_does_nothing() {
        let mut names = Interner::new();
        let mut first = Func::new(names.intern("f"));
        add(&mut first, &mut names);
        let mut second = Func::new(names.intern("g"));
        add(&mut second, &mut names);

        let text = assemble(&[first, second], &names, &target()).expect("two functions");
        assert_eq!(text.funcs[1].start, 16);
        assert_eq!(text.bytes.len(), 18);
        assert!(text.bytes[2..16].iter().all(|byte| *byte == NOP), "{:?}", text.bytes);
    }

    #[test]
    fn a_function_that_was_never_allocated_is_refused_rather_than_encoded_wrongly() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        let vreg = func.new_vreg(GPR);
        let neg = Opcode::new(names.intern("x64.neg_r_32"));
        func.build(block, neg).operand(Operand::write(vreg, GPR)).finish();
        let error = assemble(&[func], &names, &target()).expect_err("a virtual register");
        assert_eq!(
            error,
            Error::Virtual { func: "f".to_owned(), opcode: "x64.neg_r_32".to_owned() }
        );
    }

    #[test]
    fn an_opcode_the_target_does_not_describe_is_refused() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"));
        let block = func.create_block();
        let made_up = Opcode::new(names.intern("x64.frobnicate"));
        func.build(block, made_up).finish();
        let error = assemble(&[func], &names, &target()).expect_err("no such instruction");
        assert_eq!(
            error,
            Error::Opcode { func: "f".to_owned(), opcode: "x64.frobnicate".to_owned() }
        );
    }

    #[test]
    fn a_machine_with_no_encoder_here_is_said_so_rather_than_encoded_as_x86_64() {
        let names = Interner::new();
        let aarch64 = TargetInfo::new(Triple::new(Arch::Aarch64, Os::Linux, Env::Gnu));
        let error = assemble(&[], &names, &aarch64).expect_err("no encoder");
        assert!(matches!(error, Error::Machine { .. }), "{error:?}");
    }
}
