//! The selector: an IR function becomes a machine IR function.
//!
//! Design: `spec/10-backend.md` sections 10.2 and 10.3.
//!
//! What the matcher in [`crate::select`] does is answer one question about one term. What this
//! does is ask it: walk a function, decide which terms are worth asking about, and build machine
//! instructions out of what comes back. Nothing here decides what an IR term lowers to. That is
//! in `rules/x86-64.rules` and it is proved before it is used, which is the whole point of the
//! arrangement and the reason this file is short.
//!
//! # What it does with an instruction
//!
//! It tries the ways the instruction can be shown to the matcher, in order, and takes the first
//! that a rule fires on. [`crate::term`] is what a way of showing one is, and the order is the
//! most specific first: an operand that is a constant is offered as a constant before it is
//! offered as a register, and an operand computed by an instruction of its own is offered as
//! that instruction before it is offered as a register. A rule that wants an immediate too wide
//! for the machine has a guard that turns it down, and the search carries on to the way of
//! showing it that puts the constant in a register, which is the right answer and is one nobody
//! had to write down.
//!
//! A constant is not lowered where it is written. It is materialized where a register for it is
//! first wanted, which is what keeps a constant that every use folded into an immediate from
//! leaving a dead instruction behind, and it also gives the value the shortest live range it
//! could have. The instruction that materializes it comes from the rule set like everything else.
//!
//! # What it does not do yet
//!
//! Anything with an effect, and anything that ends a block. There are no rules for loads,
//! stores, calls, branches or returns, because `spec/10-backend.md` section 10.2 wants the
//! language for an effect settled before the rules that have one are written, and until they are
//! written a function containing any of them is one this reports it cannot lower. Everything is
//! in the general purpose registers, because every rule in the set is about an integer.
//!
//! Blocks are walked in the order the function holds them and a value is expected to be defined
//! before it is used. That is true of a straight line and it is what the rules cover.

use std::fmt;

use rucc_base::Interner;
use rucc_ir::{Block, Def, Func, Inst, Opcode, Value};
use rucc_mir as mir;
use rucc_target::RegClass;
use rucc_target::x86_64;

use crate::select::{Match, Piece, Rule, Table};
use crate::term::{MAX_ARGS, PLAIN, Plan, Shown, Term, Terms};

/// The prefix a rule file puts in front of a machine term, which says which target it belongs
/// to and is not part of the opcode.
const PREFIX: &str = "x64.";

/// Why a function could not be lowered.
///
/// One instruction and then nothing. A function with no rule for something in it is a function
/// this cannot finish, and the second thing it could not lower is not news.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unsupported {
    /// The instruction that stopped it.
    pub inst: Inst,
    /// What the rule file would call it, or nothing if the rule language has no name for it at
    /// all, which is what an instruction at a width nothing is written about looks like.
    pub term: Option<&'static str>,
}

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.term {
            Some(term) => write!(f, "no rule lowers `{term}`"),
            None => f.write_str("no rule lowers this instruction"),
        }
    }
}

impl std::error::Error for Unsupported {}

/// The x86-64 machine IR for that function.
///
/// # Errors
///
/// The first instruction no rule fires on, which today is every load, every store, every call
/// and every terminator.
pub fn func(source: &Func, names: &mut Interner) -> Result<mir::Func, Unsupported> {
    Lowering::new(source, names).run()
}

/// One function being lowered.
struct Lowering<'a> {
    source: &'a Func,
    names: &'a mut Interner,
    out: mir::Func,
    /// The machine register each IR value is in, once it has one.
    regs: Vec<Option<mir::Reg>>,
    /// How many times each IR value is read, which is what says whether an instruction may be
    /// folded into the one that reads it.
    uses: Vec<u32>,
    /// The block being filled.
    at: Option<mir::Block>,
    /// The class everything is in until there is a rule about a float.
    gpr: RegClass,
}

impl<'a> Lowering<'a> {
    fn new(source: &'a Func, names: &'a mut Interner) -> Self {
        let counts = source.counts();
        let name = source.name;
        let mut uses = vec![0; counts.values];
        for block in source.blocks() {
            for inst in source.insts(block) {
                for &arg in &source[source[inst].args] {
                    uses[arg.index()] += 1;
                }
                for call in source.successors(inst) {
                    for &arg in &source[call.args] {
                        uses[arg.index()] += 1;
                    }
                }
            }
        }
        Self {
            source,
            names,
            out: mir::Func::new(name),
            regs: vec![None; counts.values],
            uses,
            at: None,
            gpr: x86_64::GPR,
        }
    }

    fn run(mut self) -> Result<mir::Func, Unsupported> {
        for block in self.source.blocks() {
            self.block(block)?;
        }
        Ok(self.out)
    }

    /// One block: its parameters, then every instruction in it that is not folded into another.
    fn block(&mut self, block: Block) -> Result<(), Unsupported> {
        let out = self.out.create_block();
        self.at = Some(out);
        for &param in self.source[block].params.iter() {
            let reg = self.out.append_param(out, self.gpr);
            self.regs[param.index()] = Some(reg);
        }

        // What each instruction matched, and which instructions were folded into another. The
        // instruction that is folded comes before the one that folds it, so the decision has to
        // be made for the whole block before any of it is written, and it is made backwards: an
        // instruction that has been folded into a later one does not get to fold anything into
        // itself, because the rule that took it only reached one level down.
        let insts: Vec<Inst> = self.source.insts(block).collect();
        let mut found: Vec<Option<Match<Term>>> = (0..insts.len()).map(|_| None).collect();
        let mut folded: Vec<Inst> = Vec::new();
        for (index, &inst) in insts.iter().enumerate().rev() {
            if folded.contains(&inst) {
                continue;
            }
            if let Some((plan, matched)) = self.select(inst) {
                folded.extend(self.folds(inst, plan));
                found[index] = Some(matched);
            }
        }

        for (&inst, matched) in insts.iter().zip(found) {
            if folded.contains(&inst) || self.source[inst].opcode == Opcode::IConst {
                continue;
            }
            let matched = matched.ok_or_else(|| self.unsupported(inst))?;
            self.emit(inst, &matched)?;
        }
        Ok(())
    }

    /// The rule that fires on an instruction, and what it bound.
    ///
    /// The plans are tried in order and the first that matches wins, which is the maximal munch
    /// `spec/10-backend.md` asks for: a plan that offers more to the matcher is tried before one
    /// that offers less.
    fn select(&self, inst: Inst) -> Option<(Plan, Match<Term>)> {
        for plan in self.plans(inst) {
            let terms = Terms::new(self.source, inst, plan);
            if let Some(matched) = TABLE.find(&terms, Term::Root) {
                return Some((plan, matched));
            }
        }
        None
    }

    /// Every way this instruction can be shown to the matcher, most offered first.
    fn plans(&self, inst: Inst) -> Vec<Plan> {
        let args = &self.source[self.source[inst].args];
        let mut plans = vec![PLAIN];
        for (index, &arg) in args.iter().enumerate().take(MAX_ARGS) {
            let mut ways = Vec::new();
            if self.foldable(inst, arg) {
                ways.push(Shown::Expand);
            }
            if Terms::new(self.source, inst, PLAIN).constant(arg).is_some() {
                ways.push(Shown::Const);
            }
            ways.push(Shown::Reg);
            plans = plans
                .into_iter()
                .flat_map(|plan| {
                    ways.iter().map(move |&way| {
                        let mut next = plan;
                        next[index] = way;
                        next
                    })
                })
                .collect();
        }
        plans
    }

    /// Whether an operand may be shown as the instruction that computed it.
    ///
    /// It has to be in the same block, because a rule that folds one instruction into another
    /// moves the work to where the second one is. It has to be read only by this instruction,
    /// because folding it does not delete it for anybody else and doing the work twice is not a
    /// saving. And it has to be something rather than a block parameter, and not a constant,
    /// which is shown as a constant instead.
    fn foldable(&self, into: Inst, value: Value) -> bool {
        let Def::Result { inst, .. } = self.source[value].def else { return false };
        if self.source[inst].opcode == Opcode::IConst || self.uses[value.index()] != 1 {
            return false;
        }
        self.source.block_of(inst).is_some()
            && self.source.block_of(inst) == self.source.block_of(into)
    }

    /// The instructions a match folded into the one it matched.
    ///
    /// The plan is what says this, not the bindings: a binding is a register or a number either
    /// way, and an operand shown as the instruction that computed it is one no rule could have
    /// matched without taking that instruction, because the plan offered the matcher nothing
    /// else to call it.
    fn folds(&self, inst: Inst, plan: Plan) -> Vec<Inst> {
        let args = &self.source[self.source[inst].args];
        args.iter()
            .take(MAX_ARGS)
            .enumerate()
            .filter(|&(index, _)| plan[index] == Shown::Expand)
            .filter_map(|(_, &arg)| match self.source[arg].def {
                Def::Result { inst, .. } => Some(inst),
                Def::Param { .. } => None,
            })
            .collect()
    }

    /// Build the machine instruction a match calls for.
    fn emit(&mut self, inst: Inst, matched: &Match<Term>) -> Result<(), Unsupported> {
        let rule: &Rule = TABLE.rule(matched);
        let pieces = rule.replacement;
        let Some(Piece::App { head, arity }) = pieces.first() else {
            return Err(self.unsupported(inst));
        };
        let opcode = head.strip_prefix(PREFIX).ok_or_else(|| self.unsupported(inst))?;
        let form = x86_64::form(opcode).ok_or_else(|| self.unsupported(inst))?;

        let mut read = Read::default();
        let mut at = 1;
        for _ in 0..*arity {
            at = self.read(inst, pieces, at, &matched.bindings, &mut read)?;
        }

        let descs = form.operands();
        let writes = descs.iter().take_while(|desc| desc.role.is_def()).count();
        if descs.len() - writes != read.regs.len() {
            return Err(self.unsupported(inst));
        }

        // The first thing the instruction writes is what it computes, and any others are
        // registers the machine destroys on the way, which are fresh because nothing else is in
        // them and nothing reads them.
        let result = self.source[inst].first_result.ok_or_else(|| self.unsupported(inst))?;
        let dest = self.new_reg(result);
        let mut regs = vec![dest];
        regs.extend((1..writes).map(|_| self.out.new_vreg(self.gpr)));
        regs.extend(read.regs.iter().copied());

        let block = self.at.expect("a block is being filled");
        let opcode = mir::Opcode::new(self.names.intern(head));
        let mut build = self.out.build(block, opcode).at(self.source.span(inst));
        for (desc, reg) in descs.iter().zip(regs) {
            let operand = mir::Operand {
                reg,
                class: desc.class,
                role: desc.role,
                constraint: desc.constraint,
            };
            build = build.operand(operand);
        }
        if let Some(mem) = read.mem {
            build = build.mem(mem);
        }
        if let Some(imm) = read.imm {
            build = build.imm(imm);
        }
        build.finish();
        Ok(())
    }

    /// Read one argument of a replacement, which is a register, a number or an address.
    ///
    /// Gives back the position after it, because a replacement is flat and an address takes
    /// arguments of its own.
    fn read(
        &mut self,
        inst: Inst,
        pieces: &'static [Piece],
        at: usize,
        bindings: &[Term],
        out: &mut Read,
    ) -> Result<usize, Unsupported> {
        match pieces.get(at) {
            Some(Piece::Int(value)) => {
                out.imm = i64::try_from(*value).ok();
                Ok(at + 1)
            }
            Some(Piece::Var { index, .. }) => {
                match bindings.get(*index) {
                    Some(&Term::Reg(value)) => {
                        let reg = self.reg_of(value)?;
                        out.regs.push(reg);
                    }
                    Some(&Term::Num(value)) => out.imm = i64::try_from(value).ok(),
                    // A pattern binds a register or a number and nothing else, so this is a
                    // rule the matcher and this file disagree about.
                    _ => return Err(self.unsupported(inst)),
                }
                Ok(at + 1)
            }
            Some(Piece::App { head, arity }) => {
                let kind = x86_64::address(head).ok_or_else(|| self.unsupported(inst))?;
                let mut inner = Read::default();
                let mut next = at + 1;
                for _ in 0..*arity {
                    next = self.read(inst, pieces, next, bindings, &mut inner)?;
                }
                let mem = address(kind, &inner, self.gpr).ok_or_else(|| self.unsupported(inst))?;
                out.mem = Some(mem);
                Ok(next)
            }
            None => Err(self.unsupported(inst)),
        }
    }

    /// The register a value is in, materializing it if it is a constant that has not been put in
    /// one yet.
    fn reg_of(&mut self, value: Value) -> Result<mir::Reg, Unsupported> {
        if let Some(reg) = self.regs[value.index()] {
            return Ok(reg);
        }
        let constant = match self.source[value].def {
            Def::Result { inst, .. } => {
                (self.source[inst].opcode == Opcode::IConst).then_some(inst)
            }
            Def::Param { .. } => None,
        };
        if let Some(inst) = constant {
            let matched = self
                .select(inst)
                .map(|(_, matched)| matched)
                .ok_or_else(|| self.unsupported(inst))?;
            self.emit(inst, &matched)?;
            return Ok(self.regs[value.index()].expect("a constant is written into a register"));
        }
        Ok(self.new_reg(value))
    }

    /// A fresh register for a value, which is what the instruction computing it writes.
    fn new_reg(&mut self, value: Value) -> mir::Reg {
        if let Some(reg) = self.regs[value.index()] {
            return reg;
        }
        let reg = self.out.new_vreg(self.gpr);
        self.regs[value.index()] = Some(reg);
        reg
    }

    fn unsupported(&self, inst: Inst) -> Unsupported {
        Unsupported { inst, term: Terms::new(self.source, inst, PLAIN).name(inst) }
    }
}

/// What the arguments of one replacement came to.
#[derive(Debug, Default)]
struct Read {
    regs: Vec<mir::Reg>,
    imm: Option<i64>,
    mem: Option<mir::Mem>,
}

/// The addressing mode an address constructor's arguments make.
fn address(kind: x86_64::Address, read: &Read, gpr: RegClass) -> Option<mir::Mem> {
    let scale = u8::try_from(read.imm?).ok()?;
    let mut regs = read.regs.iter().copied();
    let first = mir::Operand::read(regs.next()?, gpr);
    if kind.has_base() {
        let index = mir::Operand::read(regs.next()?, gpr);
        return Some(mir::Mem::at(first).indexed(index, scale));
    }
    Some(mir::Mem { base: None, index: Some(first), scale, disp: 0, symbol: None })
}

/// The table this selector matches with.
///
/// One target for now, because one target has a rule file. Which table to use becomes a question
/// the moment a second one does, and the answer will be the target the session was given rather
/// than a constant here.
static TABLE: &Table = &crate::select::x86_64::TABLE;

#[cfg(test)]
mod tests {
    use rucc_ir::{Builder, Flags, Signature, Type};
    use rucc_target::x86_64::REGS;

    use super::*;

    /// A function of as many 64 bit parameters as the test wants, and the block they are in.
    fn blank(params: &[Type]) -> (Interner, Func, Block, Vec<Value>) {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let block = func.create_block();
        let values = params.iter().map(|&ty| func.append_param(block, ty)).collect();
        (names, func, block, values)
    }

    /// The machine IR text a function lowers to.
    fn lower(names: &mut Interner, source: &Func) -> String {
        let out = func(source, names).expect("every instruction has a rule");
        mir::print_func(&out, names, &REGS)
    }

    #[test]
    fn an_addition_of_two_registers_is_one_instruction() {
        let i32 = Type::int(32);
        let (mut names, mut func, block, args) = blank(&[i32, i32]);
        let mut build = Builder::new(&mut func, block);
        build.binary(Opcode::Add, args[0], args[1], Flags::default());

        assert_eq!(
            lower(&mut names, &func),
            "mfunc @f {\nblock0(%0:gpr, %1:gpr):\n    \
             %2:gpr(reuse 1) = x64.add_rr_32 %0, %1\n}\n"
        );
    }

    #[test]
    fn a_constant_operand_becomes_an_immediate() {
        let i32 = Type::int(32);
        let (mut names, mut func, block, args) = blank(&[i32]);
        let mut build = Builder::new(&mut func, block);
        let seven = build.iconst(i32, 7);
        build.binary(Opcode::Add, args[0], seven, Flags::default());

        // The constant is in the instruction and nothing was written to hold it, which is what
        // materializing one where a register for it is wanted buys.
        assert_eq!(
            lower(&mut names, &func),
            "mfunc @f {\nblock0(%0:gpr):\n    %1:gpr(reuse 1) = x64.add_ri_32 %0, 7\n}\n"
        );
    }

    #[test]
    fn a_constant_too_wide_for_an_immediate_goes_into_a_register() {
        let i64 = Type::int(64);
        let (mut names, mut func, block, args) = blank(&[i64]);
        let mut build = Builder::new(&mut func, block);
        let big = build.iconst(i64, i128::from(i32::MAX) + 1);
        build.binary(Opcode::Add, args[0], big, Flags::default());

        // Nobody wrote this fallback down. The rule that takes an immediate has a guard that
        // turns a number this wide down, so it does not fire, and the next way of showing the
        // operand puts it in a register.
        assert_eq!(
            lower(&mut names, &func),
            "mfunc @f {\nblock0(%0:gpr):\n    %1:gpr = x64.mov_ri_64 2147483648\n    \
             %2:gpr(reuse 1) = x64.add_rr_64 %0, %1\n}\n"
        );
    }

    #[test]
    fn an_index_calculation_folds_into_an_address() {
        let i64 = Type::int(64);
        let (mut names, mut func, block, args) = blank(&[i64, i64]);
        let mut build = Builder::new(&mut func, block);
        let four = build.iconst(i64, 4);
        let scaled = build.binary(Opcode::Mul, args[1], four, Flags::default());
        build.binary(Opcode::Add, args[0], scaled, Flags::default());

        // Three IR instructions and one machine instruction. The multiply is gone because the
        // rule that matched reached down and took it.
        assert_eq!(
            lower(&mut names, &func),
            "mfunc @f {\nblock0(%0:gpr, %1:gpr):\n    %2:gpr = x64.lea_64 [%0 + %1*4]\n}\n"
        );
    }

    #[test]
    fn an_instruction_read_twice_is_not_folded_into_either_reader() {
        let i64 = Type::int(64);
        let (mut names, mut func, block, args) = blank(&[i64, i64]);
        let mut build = Builder::new(&mut func, block);
        let four = build.iconst(i64, 4);
        let scaled = build.binary(Opcode::Mul, args[1], four, Flags::default());
        let first = build.binary(Opcode::Add, args[0], scaled, Flags::default());
        build.binary(Opcode::Add, first, scaled, Flags::default());

        // Folding it into both would compute it twice, which is not a saving, so it stays where
        // it is and both readers read the register it wrote.
        let text = lower(&mut names, &func);
        assert!(text.contains("x64.lea_64 [%1*4]"), "{text}");
        assert_eq!(text.matches("x64.add_rr_64").count(), 2, "{text}");
    }

    #[test]
    fn a_shift_by_a_register_asks_for_it_in_cl() {
        let i32 = Type::int(32);
        let (mut names, mut func, block, args) = blank(&[i32, i32]);
        let mut build = Builder::new(&mut func, block);
        build.binary(Opcode::Shl, args[0], args[1], Flags::default());

        // The fixed register is not in the rule. It is what the target says the instruction does
        // with its operands, and the allocator is what will act on it.
        let text = lower(&mut names, &func);
        assert!(text.contains("x64.shl_rcl_32 %0, %1($rcx)"), "{text}");
    }

    #[test]
    fn a_division_names_the_registers_and_the_register_it_destroys() {
        let i32 = Type::int(32);
        let (mut names, mut func, block, args) = blank(&[i32, i32]);
        let mut build = Builder::new(&mut func, block);
        build.binary(Opcode::SDiv, args[0], args[1], Flags::default());

        // Two definitions, because a division writes the remainder whether anybody wanted it or
        // not, and the second one is early because it is destroyed before the operands are read.
        let text = lower(&mut names, &func);
        assert!(
            text.contains("%2:gpr($rax), early %3:gpr($rdx) = x64.idiv_quo_32 %0($rax), %1"),
            "{text}"
        );
    }

    #[test]
    fn an_instruction_no_rule_covers_is_reported() {
        let i64 = Type::int(64);
        let (mut names, mut source, block, args) = blank(&[i64]);
        let mut build = Builder::new(&mut source, block);
        build.ret(&[args[0]]);

        let failed = func(&source, &mut names).expect_err("nothing lowers a return yet");
        assert_eq!(failed.to_string(), "no rule lowers this instruction");
    }
}
