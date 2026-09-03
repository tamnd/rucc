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
//! Anything that branches, and anything that calls. Loads and stores are lowered, and so is a
//! return, but there are no rules for calls or for the branches and so no terms offered for them.
//! A function containing one is a function this reports it cannot lower rather than one it lowers
//! wrongly. Everything is in the general purpose registers, because every rule in the set is
//! about an integer, so a function that returns a `double` is one of those too.
//!
//! A store and a return are the two things here that write no register. A store is emitted like
//! everything else and the only difference is that there is no result to put anywhere, so the
//! operands the target describes are all reads. A return is the same, and what it is for is its
//! one operand: the target constrains it to the register the caller reads the value out of, and
//! the allocator is what gets it there. The instruction that leaves is not chosen here at all,
//! because the epilogue has to give the frame back first and [`crate::finish`] writes that after
//! allocation, so a return of nothing is lowered to nothing.
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
/// The first instruction no rule fires on, which today is every call and every branch, and
/// anything at a width the rule set is not written at.
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
            if folded.contains(&inst) || self.writes_nothing(inst) {
                continue;
            }
            let matched = matched.ok_or_else(|| self.unsupported(inst))?;
            self.emit(inst, &matched)?;
        }
        Ok(())
    }

    /// Whether an instruction is one no machine instruction is written for where it stands.
    ///
    /// Two of them, and neither is a lowering decision, which is why neither is a rule. A
    /// constant is written where a register for it is first wanted rather than where the IR put
    /// it, and every reader of one may have folded it into an immediate, in which case nowhere is
    /// the right place. A return of nothing has nothing to put anywhere: the epilogue gives the
    /// frame back and leaves, and it is appended to every block with no successors long after
    /// this has finished, so a return with a value is one instruction here and a return without
    /// one is none.
    fn writes_nothing(&self, inst: Inst) -> bool {
        let data = &self.source[inst];
        data.opcode == Opcode::IConst
            || (data.opcode == Opcode::Return && self.source[data.args].is_empty())
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
        // them and nothing reads them. An instruction that writes nothing at all is one whose
        // whole purpose is its effect, which is what a store is, and there is no result to put
        // anywhere.
        let mut regs = Vec::new();
        if writes > 0 {
            let result = self.source[inst].first_result.ok_or_else(|| self.unsupported(inst))?;
            regs.push(self.new_reg(result));
            regs.extend((1..writes).map(|_| self.out.new_vreg(self.gpr)));
        } else if self.source[inst].first_result.is_some() {
            // A rule that throws away a value the IR gave a name to would leave every reader of
            // that name with nothing to read, so it is a rule this and the target disagree about.
            return Err(self.unsupported(inst));
        }
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
///
/// One arm per constructor rather than a question asked of the kind, because what the arguments
/// mean is the whole of what tells the four apart: the same register is a base in one and an
/// index in another, and the same constant is a scale in one and a displacement in another.
fn address(kind: x86_64::Address, read: &Read, gpr: RegClass) -> Option<mir::Mem> {
    let mut regs = read.regs.iter().copied().map(|reg| mir::Operand::read(reg, gpr));
    match kind {
        x86_64::Address::BaseIndexScale => {
            let base = regs.next()?;
            let index = regs.next()?;
            Some(mir::Mem::at(base).indexed(index, u8::try_from(read.imm?).ok()?))
        }
        x86_64::Address::IndexScale => Some(mir::Mem {
            base: None,
            index: Some(regs.next()?),
            scale: u8::try_from(read.imm?).ok()?,
            disp: 0,
            symbol: None,
        }),
        x86_64::Address::Base => Some(mir::Mem::at(regs.next()?)),
        // The rule that writes this has a guard saying the constant fits, so a displacement that
        // does not is a rule and a target that disagree rather than a program this cannot compile.
        x86_64::Address::BaseOffset => {
            Some(mir::Mem { disp: i32::try_from(read.imm?).ok()?, ..mir::Mem::at(regs.next()?) })
        }
    }
}

/// The table this selector matches with.
///
/// One target for now, because one target has a rule file. Which table to use becomes a question
/// the moment a second one does, and the answer will be the target the session was given rather
/// than a constant here.
static TABLE: &Table = &crate::select::x86_64::TABLE;

#[cfg(test)]
mod tests {
    use rucc_ir::{Builder, Flags, MemInfo, MemOrder, Signature, Type};
    use rucc_regalloc::assign::Env;
    use rucc_target::x86_64::{FRAME, REGS, SYSV};

    use super::*;
    use crate::finish::finish;
    use crate::frame::{Frame, Layout};

    /// A function of as many 64 bit parameters as the test wants, and the block they are in.
    fn blank(params: &[Type]) -> (Interner, Func, Block, Vec<Value>) {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let block = func.create_block();
        let values = params.iter().map(|&ty| func.append_param(block, ty)).collect();
        (names, func, block, values)
    }

    /// An ordinary access: not atomic, and aligned enough that nothing here has an opinion.
    /// Neither field reaches selection, which is the point of saying it once here.
    fn plain() -> MemInfo {
        MemInfo { size: 0, align: 1, order: MemOrder::NotAtomic, tbaa: None }
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
    fn a_load_reads_through_the_register_the_address_is_in() {
        let i64 = Type::int(64);
        let (mut names, mut func, block, args) = blank(&[i64]);
        let mut build = Builder::new(&mut func, block);
        build.load(Type::int(32), args[0], plain(), Flags::default());

        assert_eq!(
            lower(&mut names, &func),
            "mfunc @f {\nblock0(%0:gpr):\n    %1:gpr = x64.mov_rm_32 [%0]\n}\n"
        );
    }

    #[test]
    fn a_store_writes_no_register_and_the_value_it_writes_is_the_one_the_ir_gave_it() {
        let (mut names, mut func, block, args) = blank(&[Type::int(32), Type::int(64)]);
        let mut build = Builder::new(&mut func, block);
        build.store(args[0], args[1], plain(), Flags::default());

        // The value is the first parameter and the address is the second, and the instruction
        // takes them the other way round. Getting that backwards would compile to a store of the
        // address into the value, which is a program that runs and does the wrong thing.
        assert_eq!(
            lower(&mut names, &func),
            "mfunc @f {\nblock0(%0:gpr, %1:gpr):\n    x64.mov_mr_32 %0, [%1]\n}\n"
        );
    }

    #[test]
    fn an_address_with_a_constant_added_folds_into_the_access() {
        let i64 = Type::int(64);
        let (mut names, mut func, block, args) = blank(&[i64]);
        let mut build = Builder::new(&mut func, block);
        let twelve = build.iconst(i64, 12);
        let field = build.binary(Opcode::Add, args[0], twelve, Flags::default());
        build.load(Type::int(64), field, plain(), Flags::default());

        // Two IR instructions and one machine instruction, which is what every read of a field
        // of a structure comes to.
        assert_eq!(
            lower(&mut names, &func),
            "mfunc @f {\nblock0(%0:gpr):\n    %1:gpr = x64.mov_rm_64 [%0 + 12]\n}\n"
        );
    }

    #[test]
    fn a_displacement_too_wide_to_encode_leaves_the_addition_where_it_is() {
        let i64 = Type::int(64);
        let (mut names, mut func, block, args) = blank(&[i64]);
        let mut build = Builder::new(&mut func, block);
        let big = build.iconst(i64, i128::from(i32::MAX) + 1);
        let far = build.binary(Opcode::Add, args[0], big, Flags::default());
        build.load(Type::int(32), far, plain(), Flags::default());

        // A displacement is signed and 32 bits. The rule that folds one has a guard that turns
        // this down, so the addition stays and the load reads through what it produced. Nobody
        // wrote that fallback: it is the next way of showing the operand.
        let text = lower(&mut names, &func);
        assert!(text.contains("x64.mov_rm_32 [%2]"), "{text}");
        assert!(text.contains("x64.add_rr_64"), "{text}");
    }

    #[test]
    fn a_store_of_a_value_that_was_loaded_is_two_instructions_and_no_arithmetic() {
        let i64 = Type::int(64);
        let (mut names, mut func, block, args) = blank(&[i64, i64]);
        let mut build = Builder::new(&mut func, block);
        let got = build.load(Type::int(8), args[0], plain(), Flags::default());
        build.store(got, args[1], plain(), Flags::default());

        // A load feeding a store is the one place folding would be wrong: an x86-64 `mov` has at
        // most one memory operand, and there is no rule that takes two, so the load is left where
        // it is and the store reads the register it wrote.
        assert_eq!(
            lower(&mut names, &func),
            "mfunc @f {\nblock0(%0:gpr, %1:gpr):\n    %2:gpr = x64.mov_rm_8 [%0]\n    \
             x64.mov_mr_8 %2, [%1]\n}\n"
        );
    }

    #[test]
    fn an_access_at_a_width_no_rule_is_written_at_is_reported() {
        let i64 = Type::int(64);
        let (mut names, mut source, block, args) = blank(&[i64]);
        let mut build = Builder::new(&mut source, block);
        build.load(Type::int(128), args[0], plain(), Flags::default());

        let failed = func(&source, &mut names).expect_err("nothing loads 128 bits");
        assert_eq!(failed.to_string(), "no rule lowers this instruction");
    }

    #[test]
    fn a_return_asks_for_the_value_in_the_register_the_caller_reads() {
        let (mut names, mut func, block, args) = blank(&[Type::int(32)]);
        let mut build = Builder::new(&mut func, block);
        build.ret(&[args[0]]);

        // The register is not in the rule, the same way `cl` is not in the rule for a shift. It
        // is what the target says the instruction does with its operand, and the allocator is
        // what will act on it. There is no `ret` here, because giving the frame back has to
        // happen between this and leaving and the frame is not worked out yet.
        assert_eq!(
            lower(&mut names, &func),
            "mfunc @f {\nblock0(%0:gpr):\n    x64.ret_val_32 %0($rax)\n}\n"
        );
    }

    #[test]
    fn a_return_of_a_constant_puts_it_in_a_register_first() {
        let (mut names, mut func, block, _) = blank(&[]);
        let mut build = Builder::new(&mut func, block);
        let zero = build.iconst(Type::int(32), 0);
        build.ret(&[zero]);

        // No rule returns an immediate, so the plan that offers one is turned down and the next
        // one materializes it. That is `int main(void) { return 0; }` in full, once the epilogue
        // is appended to it.
        assert_eq!(
            lower(&mut names, &func),
            "mfunc @f {\nblock0:\n    %0:gpr = x64.mov_ri_32 0\n    x64.ret_val_32 %0($rax)\n}\n"
        );
    }

    #[test]
    fn a_return_of_nothing_is_no_instruction_at_all() {
        let (mut names, mut func, block, _) = blank(&[]);
        let mut build = Builder::new(&mut func, block);
        build.ret(&[]);

        // Every part of leaving a function that returns nothing is the epilogue's, and the
        // epilogue goes in after allocation. A block with nothing in it is the right answer here
        // rather than a function that could not be lowered.
        assert_eq!(lower(&mut names, &func), "mfunc @f {\nblock0:\n}\n");
    }

    #[test]
    fn the_allocator_is_what_moves_the_answer_into_the_return_register() {
        let (mut names, mut source, block, _) = blank(&[]);
        let mut build = Builder::new(&mut source, block);
        let zero = build.iconst(Type::int(32), 0);
        build.ret(&[zero]);

        let mut out = func(&source, &mut names).expect("every instruction has a rule");
        let env = Env::new().with(x86_64::GPR, SYSV.int_order, &[]);
        let allocation = rucc_regalloc::run(&mut out, &env);
        let frame = Frame::of(&out, &allocation, &Layout::new(&SYSV, REGS));
        finish(&mut out, &allocation, &frame, &SYSV, &FRAME, &mut names);

        // `int main(void) { return 0; }` end to end. Nothing here asked for `rax`: the rule said
        // the value goes back, the target said where, and the allocator is what made it true. The
        // epilogue is what leaves, and this function needs no frame, so it is the return alone.
        //
        // The copy is a register allocator that takes no hints. It hands `%0` a register at the
        // instruction that writes it, where it does not yet know that a later use insists on
        // `rax`, and `rax` is not free to hand out because that later use is holding it. So the
        // value goes somewhere else and is copied in. Every division and every shift by a
        // register already pays the same thing, and paying it once per return is what makes it
        // worth fixing rather than a new problem.
        assert_eq!(
            mir::print_func(&out, &names, &REGS),
            "mfunc @f {\nblock0:\n    $rcx = x64.mov_ri_32 0\n    $rax = x64.mov_rr_64 $rcx\n    \
             x64.ret_val_32 $rax($rax)\n    x64.ret\n}\n"
        );
    }

    #[test]
    fn an_instruction_no_rule_covers_is_reported() {
        let i64 = Type::int(64);
        let (mut names, mut source, block, args) = blank(&[i64, i64]);
        let mut build = Builder::new(&mut source, block);
        build.ret(&[args[0], args[1]]);

        // Two values back at once. Where each of them goes is the convention's answer rather than
        // a term's, so the rule language has no name for it and no rule fires.
        let failed = func(&source, &mut names).expect_err("nothing returns two values");
        assert_eq!(failed.to_string(), "no rule lowers this instruction");
    }
}
