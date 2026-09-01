//! SSA construction, by the algorithm of Braun and others.
//!
//! Design: `spec/08-ir.md` section 8.5.
//!
//! The classical way to get SSA out of a C front end is to give every local variable a stack
//! slot, emit a load for every read and a store for every write, and then run a pass that
//! builds dominance frontiers and deletes almost all of it again. That pass is the only one
//! `-O0` runs, and everything it deletes was allocated first. So we do not build it: a local
//! whose address is never taken never gets a slot, and the value it holds is worked out here
//! while the tree is being walked.
//!
//! The algorithm is Braun, Buchwald, Hack, Leissa, Mallon and Zwinkau, "Simple and Efficient
//! Construction of Static Single Assignment Form" (CC 2013). Writing a variable records the
//! value it now holds in the block doing the writing. Reading one in a block that wrote it is
//! a lookup. Reading one in a block that did not is a question for the predecessors, and the
//! answer is either the one value they all agree on or a new block parameter that collects
//! what each of them has.
//!
//! # Sealing
//!
//! The one thing the caller has to get right. A block is sealed when it will get no further
//! predecessors, and reading a variable in an unsealed block cannot ask the predecessors
//! because they are not all there yet. It gets a block parameter instead, which is filled in
//! when the block is sealed. That is what a loop header needs and is the whole reason the
//! algorithm handles loops without a dominance computation: the header is created, left
//! unsealed while its body is walked, and sealed when the back edge has been emitted.
//!
//! A block with no back edge into it can be sealed as soon as it is created, and the walk
//! seals almost everything immediately.
//!
//! # Block parameters, not phi nodes
//!
//! A phi in the paper is a block parameter here, and adding an operand to one is appending an
//! argument to the branch in each predecessor. The paper's removal of trivial phis is done in
//! two halves: a parameter found to stand for one value is recorded as standing for it, and
//! nothing is deleted until [`Ssa::finish`], which resolves every operand in the function once
//! and then drops the parameters and the arguments that went with them. One pass over the
//! function rather than one walk per removal, and no use lists to keep in step.

use std::collections::HashMap;

use rucc_base::Idx;
use rucc_diag::Span;
use rucc_ir::{Block, BlockCall, Extra, Func, Imm, Inst, InstData, Opcode, Type, Value};

/// A variable, which is somewhere in the source that can be written more than once.
///
/// What it names is the caller's business. The walk over the typed tree makes one of these per
/// local whose address is never taken, and nothing here looks inside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Var(u32);

impl Var {
    /// The variable with that number.
    #[must_use]
    pub const fn new(raw: u32) -> Var {
        Var(raw)
    }

    /// Its number.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// One edge into a block: where it comes from, and the branch target that carries its
/// arguments.
///
/// The target is named by its place in the function's table rather than by the block it goes
/// to, because an edge that has to grow an argument later needs to be found again, and two
/// edges to the same block are the same block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Edge {
    from: Block,
    call: Idx<BlockCall>,
}

/// A block parameter that stands for a variable, which is what the paper calls a phi.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Phi {
    block: Block,
    var: Var,
}

/// The state of an SSA construction over one function.
#[derive(Debug)]
pub struct Ssa {
    /// The integer type a pointer has the width of, for the one value this has to invent.
    address: Type,
    /// What each variable holds at the end of each block.
    defs: HashMap<(Var, Block), Value>,
    /// Whether each block will get more predecessors.
    sealed: Vec<bool>,
    /// The parameters of each unsealed block that are waiting for its predecessors.
    incomplete: Vec<Vec<(Var, Value)>>,
    /// The edges into each block.
    preds: Vec<Vec<Edge>>,
    /// Which block parameters are ours, and what they stand for.
    phis: HashMap<Value, Phi>,
    /// For each value, the parameters of ours that read it. The use list the paper needs,
    /// restricted to the uses it actually walks.
    users: HashMap<Value, Vec<Value>>,
    /// What each parameter that turned out to be redundant stands for instead.
    subst: HashMap<Value, Value>,
    /// The value a read of something never written gives back, one per type.
    zero: Vec<(Type, Value)>,
}

impl Ssa {
    /// A construction over a function whose pointers are as wide as that integer type.
    ///
    /// The width is here because of one case: reading a variable that nothing has written, in
    /// a block nothing branches to. C says the value is indeterminate and
    /// `spec/08-ir.md` section 8.4 says it is unspecified but stable, so this hands back a
    /// zero, and a zero of pointer type is an integer zero cast to one.
    #[must_use]
    pub fn new(address: Type) -> Ssa {
        Ssa {
            address,
            defs: HashMap::new(),
            sealed: Vec::new(),
            incomplete: Vec::new(),
            preds: Vec::new(),
            phis: HashMap::new(),
            users: HashMap::new(),
            subst: HashMap::new(),
            zero: Vec::new(),
        }
    }

    /// Records that a variable holds a value from here to the end of the block.
    pub fn write(&mut self, var: Var, block: Block, value: Value) {
        self.defs.insert((var, block), value);
    }

    /// The value a variable holds at this point in a block, which is the whole algorithm.
    ///
    /// The type is what a parameter would be given if one has to be made. It is passed in
    /// rather than remembered per variable because the caller has it in hand and a variable
    /// whose type this had to store would be a variable this had to be told about first. A
    /// variable read at two types is a variable read wrong, and what comes back is whatever
    /// the first read decided.
    ///
    /// What comes back may be a parameter that [`Ssa::finish`] later takes out. Putting it
    /// into the function is safe, because finish rewrites everything the function holds.
    /// Remembering it on the side and comparing it to something afterwards is not.
    ///
    /// # Panics
    ///
    /// Panics if the function has no entry block, which can only happen when nothing has been
    /// built into it yet.
    pub fn read(&mut self, func: &mut Func, var: Var, block: Block, ty: Type) -> Value {
        // A run of blocks with one predecessor each is walked rather than recursed through.
        // It is the shape a sequence of `if (c) return;` leaves behind, there can be thousands
        // of them in one function, and the recursion the paper is written with would be that
        // deep.
        let mut chain = Vec::new();
        let mut at = block;
        let value = loop {
            if let Some(&value) = self.defs.get(&(var, at)) {
                break self.resolve(value);
            }
            self.reserve(at);
            if !self.sealed[at.index()] {
                break self.pending(func, var, at, ty);
            }
            match self.preds[at.index()].len() {
                // Nothing reaches here, so nothing wrote it on the way.
                0 => break self.undefined(func, ty),
                // One predecessor is not a choice, so it needs no parameter to record one.
                1 => {
                    chain.push(at);
                    at = self.preds[at.index()][0].from;
                }
                _ => break self.phi(func, var, at, ty),
            }
        };
        for at in chain {
            self.write(var, at, value);
        }
        self.write(var, block, value);
        value
    }

    /// Records the edges a terminator makes, which is what tells this the shape of the CFG.
    ///
    /// Every terminator has to be handed over, and before the block it goes to is sealed. A
    /// branch this was not told about is a predecessor that will be missed, and the parameter
    /// that should have collected a value from it will be short an argument, which is
    /// something the verifier says out loud rather than something that goes quiet.
    ///
    /// # Panics
    ///
    /// Panics if the instruction is not in a block.
    pub fn branch(&mut self, func: &Func, inst: Inst) {
        let from = func.block_of(inst).expect("a terminator in a block");
        for call in func.target_list(inst).iter() {
            let to = func[call].block;
            self.reserve(to);
            self.preds[to.index()].push(Edge { from, call });
        }
    }

    /// Says that a block has all the predecessors it is going to have.
    ///
    /// # Panics
    ///
    /// Panics if the block has already been sealed.
    pub fn seal(&mut self, func: &mut Func, block: Block) {
        self.reserve(block);
        assert!(!self.sealed[block.index()], "a block is sealed once");
        self.sealed[block.index()] = true;
        // Taken rather than iterated, because filling one of these in reads variables, which
        // can leave a parameter waiting in another block but never in this one.
        let waiting = std::mem::take(&mut self.incomplete[block.index()]);
        for (var, phi) in waiting {
            let value = self.operands(func, var, phi);
            self.write(var, block, value);
        }
    }

    /// Whether a block has been told it has all its predecessors.
    #[must_use]
    pub fn is_sealed(&self, block: Block) -> bool {
        self.sealed.get(block.index()).copied().unwrap_or(false)
    }

    /// Applies everything that was worked out and drops what turned out to be redundant.
    ///
    /// Until this runs the function is correct but wordy: a parameter that stands for one
    /// value is still a parameter, and the branches still pass it. This resolves every operand
    /// of every instruction and every argument of every branch once, and then takes the
    /// parameters out along with the arguments that fed them.
    pub fn finish(mut self, func: &mut Func) {
        if self.subst.is_empty() {
            return;
        }

        let blocks: Vec<Block> = func.blocks().collect();
        for &block in &blocks {
            let insts: Vec<Inst> = func.insts(block).collect();
            for inst in insts {
                let args = func[inst].args;
                func.rewrite(args, |value| self.resolve(value));
                for call in func.target_list(inst).iter() {
                    let args = func[call].args;
                    func.rewrite(args, |value| self.resolve(value));
                }
            }
        }

        // Which positions each block is losing. Read off the function rather than off the
        // edges this was told about, so that a branch nobody mentioned still comes out with
        // arguments that match the block it goes to.
        let mut dropped: Vec<Vec<usize>> = vec![Vec::new(); func.counts().blocks];
        for &block in &blocks {
            for (index, &param) in func[block].params.iter().enumerate() {
                if self.subst.contains_key(&param) {
                    dropped[block.index()].push(index);
                }
            }
        }

        for &block in &blocks {
            let insts: Vec<Inst> = func.insts(block).collect();
            for inst in insts {
                for at in func.target_list(inst).iter() {
                    let mut call = func[at];
                    let going = &dropped[call.block.index()];
                    if going.is_empty() {
                        continue;
                    }
                    let kept: Vec<Value> = func[call.args]
                        .iter()
                        .copied()
                        .enumerate()
                        .filter(|(index, _)| !going.contains(index))
                        .map(|(_, value)| value)
                        .collect();
                    call.args = func.push_values(&kept);
                    func.set_block_call(at, call);
                }
            }
        }

        for &block in &blocks {
            if !dropped[block.index()].is_empty() {
                func.retain_params(block, |param| !self.subst.contains_key(&param));
            }
        }
    }

    // The parts of the algorithm.

    /// A parameter for a block that does not know its predecessors yet.
    fn pending(&mut self, func: &mut Func, var: Var, block: Block, ty: Type) -> Value {
        let phi = func.append_param(block, ty);
        self.phis.insert(phi, Phi { block, var });
        self.incomplete[block.index()].push((var, phi));
        self.write(var, block, phi);
        phi
    }

    /// A parameter for a block that has more than one predecessor, filled in at once.
    fn phi(&mut self, func: &mut Func, var: Var, block: Block, ty: Type) -> Value {
        let phi = func.append_param(block, ty);
        self.phis.insert(phi, Phi { block, var });
        // Written before the operands are read, because reading them can come back here, and
        // this is what stops a loop going round for ever.
        self.write(var, block, phi);
        self.operands(func, var, phi)
    }

    /// Gives a parameter one argument in each predecessor, and asks whether it was worth it.
    fn operands(&mut self, func: &mut Func, var: Var, phi: Value) -> Value {
        let block = self.phis[&phi].block;
        let ty = func[phi].ty;
        // By index, because reading a variable in a predecessor can add edges elsewhere. Not
        // here: the edges of a sealed block are all in, and an unsealed one is not filling
        // anything in yet.
        for index in 0..self.preds[block.index()].len() {
            let edge = self.preds[block.index()][index];
            let value = self.read(func, var, edge.from, ty);
            let mut call = func[edge.call];
            call.args = func.append_arg(call.args, value);
            func.set_block_call(edge.call, call);
            self.users.entry(value).or_default().push(phi);
        }
        self.trivial(func, phi)
    }

    /// Records a parameter as standing for one value, when that is all it ever collected.
    ///
    /// A parameter whose arguments are all one value, ignoring the ones that are the parameter
    /// itself coming round a loop, is that value written at a distance. The paper deletes it
    /// here. This records it and lets [`Ssa::finish`] do the deleting, which is what turns one
    /// walk of the function per removal into one walk of the function.
    fn trivial(&mut self, func: &mut Func, phi: Value) -> Value {
        let block = self.phis[&phi].block;
        let Some(at) = func[block].params.iter().position(|&param| param == phi) else {
            return phi;
        };

        let mut same: Option<Value> = None;
        for index in 0..self.preds[block.index()].len() {
            let edge = self.preds[block.index()][index];
            let arg = self.resolve(func[func[edge.call].args][at]);
            if arg == phi || same == Some(arg) {
                continue;
            }
            if same.is_some() {
                // Two values reach here, so the parameter is what says which.
                return phi;
            }
            same = Some(arg);
        }

        let same = match same {
            Some(value) => value,
            // No arguments at all, so nothing wrote the variable on any path that reaches
            // here, and this is the same case as reading it in a block with no predecessors.
            None => self.undefined(func, func[phi].ty),
        };
        self.subst.insert(phi, same);

        // Whoever read this parameter now reads what it stands for, and one of them may have
        // been holding on for this one value.
        let users = self.users.remove(&phi).unwrap_or_default();
        let inherited: Vec<Value> = users.iter().copied().filter(|&user| user != phi).collect();
        self.users.entry(same).or_default().extend(inherited.iter().copied());
        for user in inherited {
            if !self.subst.contains_key(&user) {
                self.trivial(func, user);
            }
        }
        self.resolve(same)
    }

    /// What a value stands for, after every parameter along the way has been resolved.
    ///
    /// The walk terminates because a parameter is recorded as standing for something exactly
    /// once and what it stands for was already resolved when it was recorded, so the chains
    /// grow at the far end and never close on themselves.
    fn resolve(&mut self, value: Value) -> Value {
        let mut at = value;
        while let Some(&next) = self.subst.get(&at) {
            at = next;
        }
        if at != value {
            self.subst.insert(value, at);
        }
        at
    }

    /// The value of a variable nothing wrote, which is a zero at the top of the entry block.
    ///
    /// One per type, so that two reads of the same uninitialized variable give the same
    /// answer, which is what `spec/08-ir.md` means by unspecified but stable.
    fn undefined(&mut self, func: &mut Func, ty: Type) -> Value {
        if let Some(&(_, value)) = self.zero.iter().find(|&&(at, _)| at == ty) {
            return value;
        }

        let entry = func.entry().expect("a function with a block in it");
        let first = func.insts(entry).next();
        let value = if ty.is_ptr() {
            let int = self.constant(func, entry, first, self.address);
            let args = func.push_values(&[int]);
            let cast = func.create_inst(
                InstData { args, ..InstData::new(Opcode::IntToPtr) },
                &[ty],
                Span::DUMMY,
            );
            place(func, entry, first, cast);
            func[cast].first_result.expect("one result")
        } else {
            self.constant(func, entry, first, ty)
        };

        self.zero.push((ty, value));
        value
    }

    /// A zero of an arithmetic type, at the top of the entry block.
    fn constant(&mut self, func: &mut Func, entry: Block, first: Option<Inst>, ty: Type) -> Value {
        let imm = if ty.lane().is_float() { Imm::from_bits(0) } else { Imm::int(0, ty.lane()) };
        let imm = func.add_imm(imm);
        let opcode = if ty.lane().is_float() { Opcode::FConst } else { Opcode::IConst };
        let inst = func.create_inst(
            InstData { extra: Extra::Imm(imm), ..InstData::new(opcode) },
            &[ty],
            Span::DUMMY,
        );
        place(func, entry, first, inst);
        func[inst].first_result.expect("one result")
    }

    /// Makes room for a block this has not been told about before.
    fn reserve(&mut self, block: Block) {
        let wanted = block.index() + 1;
        if self.sealed.len() < wanted {
            self.sealed.resize(wanted, false);
            self.incomplete.resize_with(wanted, Vec::new);
            self.preds.resize_with(wanted, Vec::new);
        }
    }
}

/// Puts an instruction at the top of the entry block, before whatever was first.
fn place(func: &mut Func, entry: Block, first: Option<Inst>, inst: Inst) {
    match first {
        Some(first) => func.insert_before(inst, first),
        None => func.append_inst(entry, inst),
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{Builder, Flags, IntPred, Module, Signature, print_func, verify_func};
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    use super::*;

    const I32: Type = Type::int(32);
    const BOOL: Type = Type::int(1);

    fn target() -> TargetInfo {
        TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu))
    }

    /// The function as text, after the verifier has agreed that it is one.
    ///
    /// Both halves matter and neither says what the other says. The verifier says the result is
    /// a function the rest of the compiler may believe, and the text says which values the
    /// algorithm decided on, which is the part a person has to read to know it did the right
    /// thing rather than merely a consistent one.
    fn checked(func: Func, names: &mut Interner) -> String {
        let mut module = Module::new(names.intern("t.c"), &target());
        let id = module.add_func(func);
        if let Err(errors) = verify_func(&module, &module[id], names) {
            let listed: Vec<String> = errors.iter().map(ToString::to_string).collect();
            panic!("{}", listed.join("\n"));
        }
        print_func(&module, &module[id], names)
    }

    /// A function taking one condition and returning an `i32`, with its entry block sealed.
    fn start(names: &mut Interner) -> (Func, Ssa, Block, Value) {
        let signature = Signature::new().with_params(&[BOOL]).with_returns(&[I32]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let cond = func.append_param(entry, BOOL);
        let mut ssa = Ssa::new(Type::int(64));
        ssa.seal(&mut func, entry);
        (func, ssa, entry, cond)
    }

    #[test]
    fn a_variable_read_where_it_was_written_is_the_value_it_was_written() {
        let mut names = Interner::new();
        let (mut func, mut ssa, entry, _) = start(&mut names);
        let x = Var::new(0);

        let one = Builder::new(&mut func, entry).iconst(I32, 1);
        ssa.write(x, entry, one);
        let read = ssa.read(&mut func, x, entry, I32);
        assert_eq!(read, one);

        Builder::new(&mut func, entry).ret(&[read]);
        ssa.finish(&mut func);
        assert!(func[entry].params.len() == 1, "no parameter was needed");
    }

    #[test]
    fn a_variable_written_on_both_arms_arrives_as_a_block_parameter() {
        let mut names = Interner::new();
        let (mut func, mut ssa, entry, cond) = start(&mut names);
        let x = Var::new(0);

        let then = func.create_block();
        let otherwise = func.create_block();
        let join = func.create_block();

        let branch = Builder::new(&mut func, entry).br_if(cond, then, &[], otherwise, &[]);
        ssa.branch(&func, branch);
        ssa.seal(&mut func, then);
        ssa.seal(&mut func, otherwise);

        let one = Builder::new(&mut func, then).iconst(I32, 1);
        ssa.write(x, then, one);
        let jump = Builder::new(&mut func, then).jump(join, &[]);
        ssa.branch(&func, jump);

        let two = Builder::new(&mut func, otherwise).iconst(I32, 2);
        ssa.write(x, otherwise, two);
        let jump = Builder::new(&mut func, otherwise).jump(join, &[]);
        ssa.branch(&func, jump);

        ssa.seal(&mut func, join);
        let read = ssa.read(&mut func, x, join, I32);
        Builder::new(&mut func, join).ret(&[read]);
        ssa.finish(&mut func);

        assert_eq!(checked(func, &mut names), DIAMOND);
    }

    #[test]
    fn a_variable_both_arms_agree_about_needs_no_block_parameter() {
        let mut names = Interner::new();
        let (mut func, mut ssa, entry, cond) = start(&mut names);
        let x = Var::new(0);

        let one = Builder::new(&mut func, entry).iconst(I32, 1);
        ssa.write(x, entry, one);

        let then = func.create_block();
        let otherwise = func.create_block();
        let join = func.create_block();

        let branch = Builder::new(&mut func, entry).br_if(cond, then, &[], otherwise, &[]);
        ssa.branch(&func, branch);
        ssa.seal(&mut func, then);
        ssa.seal(&mut func, otherwise);

        for block in [then, otherwise] {
            let jump = Builder::new(&mut func, block).jump(join, &[]);
            ssa.branch(&func, jump);
        }

        ssa.seal(&mut func, join);
        let read = ssa.read(&mut func, x, join, I32);
        assert_eq!(read, one, "the parameter stood for the one value both arms had");
        Builder::new(&mut func, join).ret(&[read]);
        ssa.finish(&mut func);

        assert!(func[join].params.is_empty(), "the parameter was taken out again");
        assert_eq!(checked(func, &mut names), AGREED);
    }

    #[test]
    fn a_variable_a_loop_changes_is_carried_by_the_headers_parameter() {
        let mut names = Interner::new();
        let (mut func, mut ssa, entry, _) = start(&mut names);
        let x = Var::new(0);

        let zero = Builder::new(&mut func, entry).iconst(I32, 0);
        ssa.write(x, entry, zero);

        let header = func.create_block();
        let body = func.create_block();
        let exit = func.create_block();

        let jump = Builder::new(&mut func, entry).jump(header, &[]);
        ssa.branch(&func, jump);

        // The header is left unsealed, which is the whole point: the back edge has not been
        // emitted yet and reading the variable here cannot ask the predecessors.
        let counter = ssa.read(&mut func, x, header, I32);
        let mut build = Builder::new(&mut func, header);
        let ten = build.iconst(I32, 10);
        let test = build.icmp(IntPred::Slt, counter, ten);
        let branch = build.br_if(test, body, &[], exit, &[]);
        ssa.branch(&func, branch);
        ssa.seal(&mut func, body);
        ssa.seal(&mut func, exit);

        let carried = ssa.read(&mut func, x, body, I32);
        let mut build = Builder::new(&mut func, body);
        let one = build.iconst(I32, 1);
        let next = build.binary(Opcode::Add, carried, one, Flags::NONE);
        let jump = build.jump(header, &[]);
        ssa.write(x, body, next);
        ssa.branch(&func, jump);
        ssa.seal(&mut func, header);

        let result = ssa.read(&mut func, x, exit, I32);
        Builder::new(&mut func, exit).ret(&[result]);
        ssa.finish(&mut func);

        assert_eq!(checked(func, &mut names), LOOP);
    }

    #[test]
    fn a_variable_a_loop_does_not_change_is_not_carried_at_all() {
        let mut names = Interner::new();
        let (mut func, mut ssa, entry, cond) = start(&mut names);
        let x = Var::new(0);

        let seven = Builder::new(&mut func, entry).iconst(I32, 7);
        ssa.write(x, entry, seven);

        let header = func.create_block();
        let body = func.create_block();
        let exit = func.create_block();

        let jump = Builder::new(&mut func, entry).jump(header, &[]);
        ssa.branch(&func, jump);

        let branch = Builder::new(&mut func, header).br_if(cond, body, &[], exit, &[]);
        ssa.branch(&func, branch);
        ssa.seal(&mut func, body);
        ssa.seal(&mut func, exit);

        // Read in the body, which is what makes the header need a parameter before the back
        // edge says the parameter is only ever the one value.
        let inside = ssa.read(&mut func, x, body, I32);
        let mut build = Builder::new(&mut func, body);
        build.binary(Opcode::Add, inside, inside, Flags::NONE);
        let jump = build.jump(header, &[]);
        ssa.branch(&func, jump);
        ssa.seal(&mut func, header);

        let result = ssa.read(&mut func, x, exit, I32);
        Builder::new(&mut func, exit).ret(&[result]);
        ssa.finish(&mut func);

        assert!(func[header].params.is_empty(), "the parameter went, and the addition reads %1");
        assert_eq!(checked(func, &mut names), UNCHANGED);
    }

    #[test]
    fn a_variable_two_nested_loops_do_not_change_is_carried_by_neither() {
        // The case the paper's recursive removal is for. The inner header's parameter looks
        // like it collects two values until the outer header's parameter turns out to stand
        // for one, and nothing but redoing the inner one finds that out.
        let mut names = Interner::new();
        let (mut func, mut ssa, entry, cond) = start(&mut names);
        let x = Var::new(0);

        let seven = Builder::new(&mut func, entry).iconst(I32, 7);
        ssa.write(x, entry, seven);

        let outer = func.create_block();
        let inner = func.create_block();
        let latch = func.create_block();
        let exit = func.create_block();

        let jump = Builder::new(&mut func, entry).jump(outer, &[]);
        ssa.branch(&func, jump);

        let jump = Builder::new(&mut func, outer).jump(inner, &[]);
        ssa.branch(&func, jump);

        let read = ssa.read(&mut func, x, inner, I32);
        let mut build = Builder::new(&mut func, inner);
        build.binary(Opcode::Add, read, read, Flags::NONE);
        let branch = build.br_if(cond, inner, &[], latch, &[]);
        ssa.branch(&func, branch);
        ssa.seal(&mut func, inner);
        ssa.seal(&mut func, latch);

        let branch = Builder::new(&mut func, latch).br_if(cond, outer, &[], exit, &[]);
        ssa.branch(&func, branch);
        ssa.seal(&mut func, outer);
        ssa.seal(&mut func, exit);

        let result = ssa.read(&mut func, x, exit, I32);
        Builder::new(&mut func, exit).ret(&[result]);
        ssa.finish(&mut func);

        assert!(func[outer].params.is_empty() && func[inner].params.is_empty());
        assert_eq!(checked(func, &mut names), NESTED);
    }

    #[test]
    fn a_variable_nothing_wrote_reads_as_the_same_zero_every_time() {
        let mut names = Interner::new();
        let (mut func, mut ssa, entry, _) = start(&mut names);
        let x = Var::new(0);
        let y = Var::new(1);
        let z = Var::new(2);

        let first = ssa.read(&mut func, x, entry, I32);
        let second = ssa.read(&mut func, y, entry, I32);
        let pointer = ssa.read(&mut func, z, entry, Type::PTR);
        assert_eq!(first, second, "unspecified, and the same both times");
        assert_ne!(first, pointer);

        Builder::new(&mut func, entry).ret(&[first]);
        ssa.finish(&mut func);
        assert_eq!(checked(func, &mut names), UNWRITTEN);
    }

    /// Two arms with different values, so the block below them takes a parameter.
    const DIAMOND: &str = "\
func @f(i1) -> i32, linkage(external) {
block0(%0: i1):
    br_if %0, block1, block2

block1:
    %1 = iconst.i32 1
    jump block3(%1)

block2:
    %2 = iconst.i32 2
    jump block3(%2)

block3(%3: i32):
    return %3
}
";

    /// Two arms with one value between them, so it does not.
    const AGREED: &str = "\
func @f(i1) -> i32, linkage(external) {
block0(%0: i1):
    %1 = iconst.i32 1
    br_if %0, block1, block2

block1:
    jump block3

block2:
    jump block3

block3:
    return %1
}
";

    /// A counter, which the header's parameter carries round and the body's addition adds
    /// to. The parameter is what a phi node would have been.
    const LOOP: &str = "\
func @f(i1) -> i32, linkage(external) {
block0(%0: i1):
    %1 = iconst.i32 0
    jump block1(%1)

block1(%2: i32):
    %3 = iconst.i32 10
    %4 = icmp slt %2, %3
    br_if %4, block2, block3

block2:
    %5 = iconst.i32 1
    %6 = add %2, %5
    jump block1(%6)

block3:
    return %2
}
";

    /// The same loop over a variable nothing in it writes, where the header's parameter is
    /// made, filled in from both edges, and then found to be the one value it started with.
    const UNCHANGED: &str = "\
func @f(i1) -> i32, linkage(external) {
block0(%0: i1):
    %1 = iconst.i32 7
    jump block1

block1:
    br_if %0, block2, block3

block2:
    %2 = add %1, %1
    jump block1

block3:
    return %1
}
";

    /// Two nested loops over a variable neither writes. The inner header's parameter is only
    /// found to be redundant after the outer one is, which is the recursion in the paper.
    const NESTED: &str = "\
func @f(i1) -> i32, linkage(external) {
block0(%0: i1):
    %1 = iconst.i32 7
    jump block1

block1:
    jump block2

block2:
    %2 = add %1, %1
    br_if %0, block2, block3

block3:
    br_if %0, block1, block4

block4:
    return %1
}
";

    /// A read of something nothing wrote, twice for one type and once for a pointer, which is
    /// two constants at the top of the entry block and a cast for the pointer.
    const UNWRITTEN: &str = "\
func @f(i1) -> i32, linkage(external) {
block0(%0: i1):
    %1 = iconst.i64 0
    %2 = inttoptr.ptr %1
    %3 = iconst.i32 0
    return %3
}
";
}
