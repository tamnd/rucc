//! Stackify: the values that stay on the operand stack and have no local.
//!
//! This is section 8.2 of the WebAssembly notes. A value that has one use, in the block that
//! makes it, needs no local when its instruction can move down to the place where the use pushes
//! it. The selector then writes the instruction there, as one node of an expression tree, and the
//! `local.set` and the `local.get` go away. LLVM's `WebAssemblyRegStackify` makes the same trees.
//!
//! Each instruction of a tree moves down to the root of the tree, which is the instruction that
//! keeps its place, so it is checked against all the instructions between its place and the root.
//! An instruction that moves into the tree of a root further on is counted there as if it stayed
//! in place, which can only refuse more. A pure instruction that cannot trap moves past anything,
//! because its operands are locals that nothing writes again in the block. A load, and a division
//! or a conversion that can trap, does not move past an instruction that writes memory or has
//! another effect. A call does not move past an instruction that reads memory, writes it, has an
//! effect or can trap. Two instructions of one tree can change their order, when the use pushes
//! its operands in an order that is not the order of the IR, and the check of the first one
//! against the second one covers that.
//!
//! Only the instructions whose code pushes each operand once take part, as a user and as an
//! instruction that moves, so the code of a moved instruction is written once. A call does not move
//! to a use that is written only on one path, which is an argument of one edge of a `br_if`, or to
//! a use that comes after the epilogue or after the buffer of the extra arguments of a variadic
//! call is written. An operand of a moved instruction is pushed where that instruction is pushed,
//! so the same limits apply to it. A block whose branch takes the `longjmp` of a call is left as it
//! is, because its call is written in a `try_table`. See `sjlj.rs`.
//!
//! A value with more than one use moves down in the same way to the first of its uses, when no
//! other instruction of the block reads it before that use, and it keeps its local. The selector
//! then writes the instruction at that use and a `local.tee` after it, and the other uses read the
//! local. This is not done for a use that is written only on one path, because then the local is
//! not written on the other path.
//!
//! The uses are counted as the code pushes the operands, so a load or a store whose address is
//! folded into its offset field uses the base address and not the `ptr_add`. A `ptr_add` that is
//! left with no use is not written at all.

use rucc_base::hash::{Map, Set};
use rucc_ir::{Extra, Flags, FloatPred, Func, Inst, Opcode, Value};

use super::{Lower, builtin, pair};
use crate::{functype, is_pair};

/// The deepest tree, which keeps the recursion of the selector small on a long chain of
/// arithmetic.
const DEPTH: u32 = 64;

/// How many instructions before a position of a block do what a moved instruction cannot pass.
#[derive(Clone, Copy, Default)]
struct Counts {
    /// Write memory, call, or have another effect.
    effect: u32,
    /// Read memory.
    reads: u32,
    /// Can trap.
    traps: u32,
}

/// How an instruction can move.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Pure, and cannot trap.
    Pure,
    /// Reads memory or can trap, and has no other effect.
    Read,
    /// A call.
    Call,
}

/// Where an operand is pushed, which tells what can move there. The order is from the place that
/// takes the most to the place that takes the least.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Place {
    /// Anything that can move.
    Any,
    /// Anything but a call.
    NoCall,
    /// Only a value with one use that is not a call, because the operand is pushed on one path.
    Edge,
}

/// The facts about one block that the choice of each tree reads.
struct Tree<'a> {
    /// The position of each instruction in the block.
    at: &'a Map<Inst, usize>,
    /// The counts before each position.
    before: &'a [Counts],
    /// How many times each value of the function is used.
    uses: &'a Map<Value, u32>,
    /// The positions in the block of the uses of each value, once for each use, in order.
    seen: &'a Map<Value, Vec<usize>>,
}

/// What stackify finds for a function.
#[derive(Default)]
pub(super) struct Trees {
    /// The values with one use that stay on the stack.
    pub(super) stacked: Set<Value>,
    /// The values with more than one use that are written at their first use with a `local.tee`,
    /// and the root of the tree of that use.
    pub(super) teed: Map<Value, Inst>,
    /// The instruction that takes each value of `teed` off the stack after the `local.tee`, and
    /// so does not read its local.
    pub(super) takers: Map<Value, Inst>,
    /// The instructions that make the values of `stacked` and `teed`, which are written where
    /// the value is pushed.
    pub(super) moved: Set<Inst>,
    /// The root of the tree of each instruction of `moved` that is written, which is where the
    /// code reads its operands.
    pub(super) roots: Map<Inst, Inst>,
}

/// Whether the code of an instruction with this opcode has no effect, reads no memory and cannot
/// trap, when its operands and results are not pairs.
fn pure(opcode: Opcode) -> bool {
    matches!(
        opcode,
        Opcode::Add
            | Opcode::Sub
            | Opcode::Mul
            | Opcode::And
            | Opcode::Or
            | Opcode::Xor
            | Opcode::Shl
            | Opcode::AShr
            | Opcode::LShr
            | Opcode::UMulHigh
            | Opcode::SMulHigh
            | Opcode::FAdd
            | Opcode::FSub
            | Opcode::FMul
            | Opcode::FDiv
            | Opcode::FNeg
            | Opcode::ICmp
            | Opcode::FCmp
            | Opcode::Select
            | Opcode::Trunc
            | Opcode::SExt
            | Opcode::ZExt
            | Opcode::FPTrunc
            | Opcode::FPExt
            | Opcode::Bitcast
            | Opcode::SIToFP
            | Opcode::UIToFP
            | Opcode::PtrToInt
            | Opcode::IntToPtr
            | Opcode::PtrAdd
            | Opcode::Ctlz
            | Opcode::Cttz
            | Opcode::Ctpop
            | Opcode::Expect
            | Opcode::IConst
            | Opcode::FConst
            | Opcode::GlobalAddr
            | Opcode::BlockAddr
            | Opcode::LifetimeEnd
            | Opcode::MemEntry
            | Opcode::Prefetch
    )
}

impl Lower<'_, '_> {
    /// The values that are written where they are used, and their instructions.
    pub(super) fn stackify(&self) -> Trees {
        let func = self.func;
        let mut uses: Map<Value, u32> = Map::default();
        for block in func.blocks() {
            for inst in func.insts(block) {
                for value in self.inputs(inst) {
                    *uses.entry(value).or_default() += 1;
                }
                for call in func.successors(inst) {
                    for &value in &func[call.args] {
                        *uses.entry(value).or_default() += 1;
                    }
                }
            }
        }
        let mut trees = Trees::default();
        // The `ptr_add` instructions with no use, and then the ones that they were the only use of.
        let mut dead: Vec<Inst> = func
            .blocks()
            .flat_map(|block| func.insts(block))
            .filter(|&inst| func[inst].opcode == Opcode::PtrAdd)
            .filter(|&inst| self.results(inst).iter().all(|v| uses.get(v).is_none_or(|&n| n == 0)))
            .collect();
        while let Some(inst) = dead.pop() {
            if !trees.moved.insert(inst) {
                continue;
            }
            trees.stacked.extend(self.results(inst));
            for value in self.inputs(inst) {
                let Some(count) = uses.get_mut(&value) else { continue };
                *count -= 1;
                let Some((def, _)) = self.def(value) else { continue };
                if *count == 0 && func[def].opcode == Opcode::PtrAdd {
                    dead.push(def);
                }
            }
        }
        for block in self.blocks() {
            let Some(term) = func.terminator(block) else { continue };
            if self.caught(term).is_some() {
                continue;
            }
            let insts: Vec<Inst> = func.insts(block).collect();
            let at: Map<Inst, usize> =
                insts.iter().enumerate().map(|(i, &inst)| (inst, i)).collect();
            // The counts before each position, so that the count between two positions is one
            // subtraction.
            let mut before = vec![Counts::default(); insts.len() + 1];
            for (i, &inst) in insts.iter().enumerate() {
                let (effect, reads, traps) = self.effects(inst);
                before[i + 1] = Counts {
                    effect: before[i].effect + u32::from(effect),
                    reads: before[i].reads + u32::from(reads),
                    traps: before[i].traps + u32::from(traps),
                };
            }
            let mut seen: Map<Value, Vec<usize>> = Map::default();
            for (i, &inst) in insts.iter().enumerate() {
                if trees.moved.contains(&inst) {
                    continue;
                }
                let edges = func.successors(inst).flat_map(|call| func[call.args].iter().copied());
                for value in self.inputs(inst).into_iter().chain(edges) {
                    seen.entry(value).or_default().push(i);
                }
            }
            let tree = Tree { at: &at, before: &before, uses: &uses, seen: &seen };
            // From the end, so that an instruction is a root only when no root after it took it.
            for (root, &inst) in insts.iter().enumerate().rev() {
                if !trees.moved.contains(&inst) {
                    self.take(inst, inst, root, 0, Place::Any, &tree, &mut trees);
                }
            }
        }
        trees
    }

    /// Put in the tree of `top`, the root at position `root`, each operand of `user` that can move
    /// there, and then the operands of each one that moves. `within` is the place where `user`
    /// itself is pushed. The code of an operand is written inside the code of its user, so an
    /// operand of a value that is pushed on one edge is also pushed on that edge only, and an
    /// operand of a value that is pushed after the epilogue is also pushed after it.
    #[allow(clippy::too_many_arguments)]
    fn take(
        &self,
        user: Inst,
        top: Inst,
        root: usize,
        depth: u32,
        within: Place,
        tree: &Tree<'_>,
        trees: &mut Trees,
    ) {
        if depth >= DEPTH {
            return;
        }
        for (value, place) in self.operands(user) {
            let place = place.max(within);
            let Some((def, _)) = self.def(value) else { continue };
            let Some(&from) = tree.at.get(&def) else { continue };
            if from >= root || trees.moved.contains(&def) {
                continue;
            }
            let one = tree.uses.get(&value) == Some(&1);
            // A value with more than one use moves only to the one use of the block that is
            // written first, and only when it is not pushed on one path.
            let first = || {
                let uses = tree.seen.get(&value).map_or(&[][..], Vec::as_slice);
                uses.iter().filter(|&&at| at > from && at <= root).count() == 1
            };
            if !one && (place == Place::Edge || is_pair(self.ty(value)) || !first()) {
                continue;
            }
            let Some(kind) = self.movable(def) else { continue };
            if (kind == Kind::Call && place != Place::Any)
                || self.results(def).as_slice() != [value]
            {
                continue;
            }
            let (low, high) = (tree.before[from + 1], tree.before[root]);
            let free = match kind {
                Kind::Pure => true,
                Kind::Read => high.effect == low.effect,
                Kind::Call => {
                    high.effect == low.effect && high.reads == low.reads && high.traps == low.traps
                }
            };
            if free {
                if one {
                    trees.stacked.insert(value);
                } else {
                    trees.teed.insert(value, top);
                    trees.takers.insert(value, user);
                }
                trees.moved.insert(def);
                trees.roots.insert(def, top);
                self.take(def, top, root, depth + 1, place, tree, trees);
            }
        }
    }

    /// Whether the code of `inst` pushes each operand once before it writes its results.
    pub(super) fn pushes_once(&self, inst: Inst) -> bool {
        !self.operands(inst).is_empty()
    }

    /// The operands that the code of `inst` pushes once each, in any order, and for each one what
    /// can move there. Nothing for an instruction whose code does something else with its
    /// operands.
    fn operands(&self, inst: Inst) -> Vec<(Value, Place)> {
        let func = self.func;
        let data = &func[inst];
        let args = self.inputs(inst);
        let all = |place: Place| args.iter().map(|&v| (v, place)).collect::<Vec<_>>();
        let pairs = args.iter().chain(&self.results(inst)).any(|&v| is_pair(self.ty(v)));
        match data.opcode {
            Opcode::Call | Opcode::CallIndirect if !self.builtin_call(inst) => {
                let Extra::Call(info) = data.extra else { return Vec::new() };
                let info = func[info];
                let sig = &func[info.signature];
                if !sig.variadic {
                    return all(Place::Any);
                }
                // The extra arguments are stored in the buffer of the frame one by one, and the
                // address of an indirect call is pushed after them, so a call there would write
                // over the buffer.
                let skip = usize::from(info.callee.is_none());
                let fixed = skip + sig.params.iter().filter(|p| !p.ty.is_mem()).count();
                let place = |i| if i >= skip && i < fixed { Place::Any } else { Place::NoCall };
                args.iter().enumerate().map(|(i, &v)| (v, place(i))).collect()
            }
            _ if pairs => Vec::new(),
            Opcode::Add
            | Opcode::Sub
            | Opcode::Mul
            | Opcode::And
            | Opcode::Or
            | Opcode::Xor
            | Opcode::Shl
            | Opcode::SDiv
            | Opcode::SRem
            | Opcode::AShr
            | Opcode::UDiv
            | Opcode::URem
            | Opcode::LShr
            | Opcode::UMulHigh
            | Opcode::SMulHigh
            | Opcode::FAdd
            | Opcode::FSub
            | Opcode::FMul
            | Opcode::FDiv
            | Opcode::FNeg
            | Opcode::ICmp
            | Opcode::Select
            | Opcode::Trunc
            | Opcode::SExt
            | Opcode::ZExt
            | Opcode::FPTrunc
            | Opcode::FPExt
            | Opcode::Bitcast
            | Opcode::FPToSI
            | Opcode::FPToUI
            | Opcode::SIToFP
            | Opcode::UIToFP
            | Opcode::PtrToInt
            | Opcode::IntToPtr
            | Opcode::PtrAdd
            | Opcode::Ctlz
            | Opcode::Cttz
            | Opcode::Ctpop
            | Opcode::Expect
            | Opcode::Store => all(Place::Any),
            Opcode::Load if !data.flags.contains(Flags::VOLATILE) => all(Place::Any),
            // A short copy or fill is loads and stores that push the addresses again for each
            // piece, and the other ones push each operand once.
            Opcode::Memcpy | Opcode::Memmove | Opcode::Memset if !self.short_bulk(inst, &args) => {
                all(Place::Any)
            }
            // The other predicates push an operand twice or not at all.
            Opcode::FCmp => match data.extra {
                Extra::FloatPred(
                    FloatPred::Oeq
                    | FloatPred::Ogt
                    | FloatPred::Oge
                    | FloatPred::Olt
                    | FloatPred::Ole
                    | FloatPred::Une
                    | FloatPred::Ugt
                    | FloatPred::Uge
                    | FloatPred::Ult
                    | FloatPred::Ule,
                ) => all(Place::Any),
                _ => Vec::new(),
            },
            // The epilogue gives the frame back before the values are pushed, and a call there
            // would put its frame on top of what this function still reads. A function with no
            // frame has nothing to give back.
            Opcode::Return if !self.sret && self.framed => all(Place::NoCall),
            Opcode::Return if !self.sret => all(Place::Any),
            // The arguments of the edges of a `br_if` are pushed only when the edge is taken, so a
            // call there would not be made on the other edge, and a local written there would not
            // be written on the other edge.
            Opcode::BrIf | Opcode::Jump => {
                let mut out = Vec::new();
                if data.opcode == Opcode::BrIf {
                    out.push((args[0], Place::Any));
                }
                let place = if data.opcode == Opcode::Jump { Place::Any } else { Place::Edge };
                for call in func.successors(inst) {
                    let params = &func[call.block].params;
                    for (&arg, &param) in func[call.args].iter().zip(params) {
                        if !func[param].ty.is_mem() && arg != param {
                            out.push((arg, place));
                        }
                    }
                }
                out
            }
            _ => Vec::new(),
        }
    }

    /// How `inst` can move when its value is an operand of another instruction, or nothing when
    /// it cannot.
    fn movable(&self, inst: Inst) -> Option<Kind> {
        let data = &self.func[inst];
        if self.operands(inst).is_empty() {
            // Only a fixed `alloca` has no operands and moves. Its address is in the frame, which
            // stays where it is until the function returns.
            let fixed = data.opcode == Opcode::Alloca && self.args(inst).is_empty();
            return fixed.then_some(Kind::Pure);
        }
        match data.opcode {
            Opcode::SDiv
            | Opcode::SRem
            | Opcode::UDiv
            | Opcode::URem
            | Opcode::FPToSI
            | Opcode::FPToUI
            | Opcode::Load => Some(Kind::Read),
            Opcode::Call | Opcode::CallIndirect => {
                let Extra::Call(info) = data.extra else { return None };
                let sig = &self.func[self.func[info].signature];
                let one = functype(sig).is_ok_and(|ty| ty.results.len() == 1);
                one.then_some(Kind::Call)
            }
            Opcode::Store | Opcode::Return | Opcode::BrIf | Opcode::Jump => None,
            _ => Some(Kind::Pure),
        }
    }

    /// Whether `inst` has an effect, reads memory, and can trap, as a moved instruction sees it.
    /// An instruction that this does not know has an effect.
    fn effects(&self, inst: Inst) -> (bool, bool, bool) {
        let data = &self.func[inst];
        let args = self.args(inst);
        let pairs = args.iter().chain(&self.results(inst)).any(|&v| is_pair(self.ty(v)));
        match data.opcode {
            _ if pairs => (true, true, true),
            Opcode::Alloca if args.is_empty() => (false, false, false),
            Opcode::SDiv
            | Opcode::SRem
            | Opcode::UDiv
            | Opcode::URem
            | Opcode::FPToSI
            | Opcode::FPToUI => (false, false, true),
            Opcode::Load if !data.flags.contains(Flags::VOLATILE) => (false, true, true),
            opcode if pure(opcode) => (false, false, false),
            _ => (true, true, true),
        }
    }

    /// Whether `inst` is a call that the selector writes as a builtin of clang, whose code can
    /// push an operand more than once.
    fn builtin_call(&self, inst: Inst) -> bool {
        let Extra::Call(info) = self.func[inst].extra else { return false };
        let callee = self.func[info].callee;
        callee.is_some_and(|name| builtin::is_builtin(self.unit.names.resolve(name)))
    }
}

/// Whether `func` can have a frame on the shadow stack, which its epilogue gives back before a
/// `return` pushes its values. This is true for each function that `plan` gives a frame, and it can
/// be true for a function that `plan` gives no frame. `plan` runs after stackify, because it makes
/// locals, so this looks only at the instructions.
pub(super) fn framed(func: &Func) -> bool {
    func.blocks().flat_map(|block| func.insts(block)).any(|inst| {
        let data = &func[inst];
        let variadic =
            matches!(data.extra, Extra::Call(info) if func[func[info].signature].variadic);
        matches!(data.opcode, Opcode::Alloca | Opcode::StackRestore)
            || variadic
            || pair::calls_runtime(data.opcode) && data.results().any(|v| is_pair(func[v].ty))
    })
}
