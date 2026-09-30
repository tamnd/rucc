//! The inliner, for the calls gcc inlines at every level, those to an `always_inline` function,
//! and from `-O1` up for the calls to a small function declared `inline`.
//!
//! Design: `spec/optimizer/33-inlining.md`, and tamnd/rucc#392.
//!
//! ```c
//! extern inline __attribute__((always_inline, gnu_inline)) int
//! printf (const char *fmt, ...)
//! {
//!   return __printf_chk (1, fmt, __builtin_va_arg_pack ());
//! }
//! ```
//!
//! `always_inline` is not a hint. gcc inlines every direct call to one of these whatever the level,
//! `-O0` included, and a header that defines one is relying on that in two ways. The first is that
//! the definition above is an inline definition, so no unit is obliged to have a copy of it out of
//! line. The second is `__builtin_va_arg_pack`, which stands for the anonymous arguments of the call
//! the body was inlined into and means nothing anywhere else. glibc's fortified headers are written
//! this way, and so is `va-arg-pack-1.c` in the torture suite.
//!
//! So this runs first, before anything else in the pipeline and at every level, since `objsize`
//! right behind it wants to see the caller's objects through the wrapper's parameters. Each
//! function is settled before it is inlined anywhere, so a body goes in with the `always_inline`
//! calls inside it already gone, and a function that reaches itself again through such calls is
//! refused rather than unrolled.
//!
//! A call is spliced in place. The block it is in is split after it, and the part after becomes a
//! block that takes the call's results as parameters. The callee's blocks are copied in with every
//! side table they point into, its entry is jumped to with the arguments, a `return` becomes a jump
//! to the second half, and an `alloca` of a fixed size goes to the caller's entry block, where the
//! verifier wants it.
//!
//! A `va_arg_pack` in the callee is the last argument of a call, since that is the one place sema
//! lets it be written, and it is replaced by the anonymous arguments of the call being inlined.
//! What makes that more than a list splice is the calling convention: the lowering has already
//! decided which of those arguments go in registers and which go in memory, and it decided for the
//! outer call. SysV x86-64 puts all of a structure in registers or none of it, so a structure that
//! travelled as two registers in the outer call and would find only one left in the inner one goes
//! to memory there instead, stored to a slot in the caller just ahead of the call. The one case
//! refused is the other way round, a small structure in memory that the inner call would have
//! room for. A call that is refused stays a call. A `va_arg_pack_len` is replaced by the count.
//!
//! What is left is the out of line copy of a function that still holds either of them, which is a
//! function nothing can emit. It becomes a declaration, which is gcc's answer too: gcc emits nothing
//! for an inline definition, so a call the inliner left goes to whatever the rest of the program
//! defines under the name, which for a glibc wrapper is the library function. A function marked
//! `inline_only` goes the same way whatever it holds, because its body was only ever here to be
//! inlined: it is an `extern inline` under GNU's reading, and its external definition is somewhere
//! else in the program.
//!
//! A `static` function marked `always_inline` whose every call went in goes the same way, since
//! nothing is left that could reach its body. gcc does not emit one either, and the intrinsic
//! headers depend on that: an operand such as `pextrd`'s lane number is an `i` constraint over a
//! parameter, which is a constant in every copy spliced into a caller and a register in the out of
//! line one, and no instruction takes its lane number in a register. A `static inline` one from
//! `-O1` up goes as well once nothing calls it, which is what gcc does and what the kernel's
//! `BUILD_BUG_ON` depends on: the call to its `error` function in the out of line copy is under a
//! condition only a caller's argument settles, and a copy that is emitted is a call that survives.
//!
//! From `-O1` up the same splice takes a call to a function declared `inline` whose body, once its
//! own calls are settled, is no larger than `max-inline-insns-single`, the limit gcc gives such a
//! callee. It is the declared half of gcc's early inliner and not the rest of it: a function
//! nobody declared `inline` is left alone however small it is, and nothing here weighs the call
//! against the growth the way section 33.4 wants the later inliner to. What it is for is the code
//! after it. A `__builtin_constant_p` in the body of such a function asks about a parameter, and
//! only once the body is where the call was can the answer be the constant the caller passed,
//! which is what gcc answers and what `bcp-1.c` checks. `-fno-inline` turns this half off and
//! leaves `always_inline` alone, which is what the flag does in gcc.
//!
//! From `-O1` up it also takes the call to a `static` function that nothing reaches any other way,
//! which is the called once rule of section 33.1 and gcc's `-finline-functions-called-once`. The
//! function need not be declared `inline` and may be as large as `max-inline-functions-called-once
//! -insns`, because once its one call is inlined nothing refers to it and it becomes a declaration,
//! which emits nothing, so the program loses a call and gains nothing. That has to happen here: the
//! lowering chose which `static` functions to emit from the source, before any call was inlined.
//! A `static` helper called from one loop is the shape
//! this is for, and it is everywhere in C, which is tamnd/rucc#1932. Reaching it any other way is a
//! second call site, a tail call, its address taken by an instruction or written into an image, or
//! an alias naming it. `used`, `noinline`, `optnone` and `naked` each keep it a call. So does a call
//! more than `max-inline-functions-called-once-loop-depth` loops deep, which is 6, as it does in
//! gcc. `-fno-inline` turns this off with the declared half, which is what gcc does, and
//! `-fno-inline-functions-called-once` turns it off alone.
//!
//! A body that takes the address of one of its own labels is copied with the label, so each copy
//! has an address of its own, which is what gcc does and what `990208-1.c` checks. A body that
//! jumps to such an address, or whose labels a static table holds, is refused, since the copy
//! would still be reaching into the original.

use rucc_base::hash::{Map, Set};
use rucc_base::{Interner, Symbol};
use rucc_cost::heuristics::{
    INLINE_CALLED_ONCE_INSNS, INLINE_CALLED_ONCE_LOOP_DEPTH, INLINE_FRAME_GROWTH,
    INLINE_FRAME_GROWTH_CONSERVE, INLINE_LARGE_FRAME, INLINE_LARGE_FRAME_CONSERVE,
};
use rucc_ir::{
    Abi, AsmInfo, AttrSet, Block, BlockCall, BlockCallList, CallInfo, Datum, Def, Drains, Extra,
    Float, Func, FuncId, Imm, Inst, InstData, Linkage, MemInfo, MemOrder, Module, Opcode, Restrict,
    Signature, SwitchInfo, Type, VaInfo, Value, ValueList,
};
use rucc_target::{Isa, TargetInfo};

use crate::Stats;
use crate::cfg::Cfg;
use crate::dom::Dominators;
use crate::loops::Loops;

/// What the step calls itself in a remark, and the name `-fno-inline` turns the declared half off
/// by.
pub const NAME: &str = "inline";

/// What `-finline-functions-called-once` and its `-fno-` form toggle, which is the called once half
/// of this step alone. Not the name of a pass.
pub const ONCE: &str = "inline-functions-called-once";

const INLINED: &str = "always_inline call inlined";

const HINT_INLINED: &str = "inline call inlined";

const ONCE_INLINED: &str = "call to a static function called once inlined";

/// Which of the two reasons a function is inlined for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `always_inline`, which is a promise.
    Always,
    /// `inline`, which is a hint taken when the body is small enough.
    Hinted,
    /// A `static` function reached by one call and no other way, whose out of line copy goes
    /// away once that call is inlined.
    Once,
}

/// Why a call to an `always_inline` function was not inlined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineFailure {
    /// The function reaches itself through calls of this kind.
    Recursive,
    /// The arguments or the results of the call are not what the body takes and gives.
    Mismatch,
    /// A parameter is a structure passed by value, whose copy the call is what makes. Only a call
    /// that is not `always_inline` is refused for this. An `always_inline` one is inlined with the
    /// copy made in the caller.
    ByValue,
    /// The body starts a variable argument list of its own, which only a frame of its own has.
    VaStart,
    /// The body jumps to a label by its address, or a static table holds one of its labels,
    /// either of which would still name the original body from the copy.
    ComputedGoto,
    /// The body calls `setjmp`, or anything else that comes back more than once, whose frame
    /// would become the caller's.
    Setjmp,
    /// The body saves the registers it was called with, which in the caller hold the caller's.
    ApplyArgs,
    /// The IR has memory SSA in it, which this step runs before.
    MemorySsa,
    /// A `va_arg_pack` whose arguments cannot be forwarded to where it is.
    Pack,
    /// The body grows the stack by an amount only known when it runs, which in a loop in the
    /// caller would grow it once for every time round. Only a hint is refused for this.
    Alloca,
    /// The body is larger than a callee declared `inline` is allowed to be.
    TooLarge,
    /// The call is inside more loops than a function called once may be inlined into.
    TooDeep,
    /// The body's locals would make the caller's frame larger than it is allowed to grow. Only a
    /// call that is not `always_inline` is refused for this.
    Frame,
    /// The call has a landing pad but not in the shape the lowering builds, an `unwound` straight
    /// after it read by the branch that ends its block, so there is no pad to hand the calls the
    /// body makes once it is copied in.
    Unwinds,
    /// The callee is built for x86-64 extensions the caller is not, so its body may use
    /// instructions the caller may not assume. gcc's words for it are the ones used.
    Target,
}

impl InlineFailure {
    /// What `-fopt-info` says about it.
    #[must_use]
    pub const fn why(self) -> &'static str {
        match self {
            Self::Recursive => "always_inline call not inlined: recursive",
            Self::Mismatch => "always_inline call not inlined: arguments do not match",
            Self::ByValue => "always_inline call not inlined: structure passed by value",
            Self::VaStart => "always_inline call not inlined: callee uses va_start",
            Self::ComputedGoto => "always_inline call not inlined: callee has a computed goto",
            Self::Setjmp => "always_inline call not inlined: callee calls setjmp",
            Self::ApplyArgs => "always_inline call not inlined: callee uses __builtin_apply_args",
            Self::MemorySsa => "always_inline call not inlined: memory SSA present",
            Self::Pack => "always_inline call not inlined: va_arg_pack cannot be forwarded",
            Self::Alloca => "always_inline call not inlined: callee calls alloca",
            Self::TooLarge => "always_inline call not inlined: callee too large",
            Self::TooDeep => "always_inline call not inlined: call inside too many loops",
            Self::Frame => "always_inline call not inlined: stack frame growth limit reached",
            Self::Unwinds => "always_inline call not inlined: call has a landing pad",
            Self::Target => "always_inline call not inlined: target specific option mismatch",
        }
    }

    /// What `-fopt-info` says about it for a call to a function that was only declared `inline`.
    #[must_use]
    pub const fn hint(self) -> &'static str {
        match self {
            Self::Recursive => "inline call not inlined: recursive",
            Self::Mismatch => "inline call not inlined: arguments do not match",
            Self::ByValue => "inline call not inlined: structure passed by value",
            Self::VaStart => "inline call not inlined: callee uses va_start",
            Self::ComputedGoto => "inline call not inlined: callee has a computed goto",
            Self::Setjmp => "inline call not inlined: callee calls setjmp",
            Self::ApplyArgs => "inline call not inlined: callee uses __builtin_apply_args",
            Self::MemorySsa => "inline call not inlined: memory SSA present",
            Self::Pack => "inline call not inlined: va_arg_pack cannot be forwarded",
            Self::Alloca => "inline call not inlined: callee calls alloca",
            Self::TooLarge => "inline call not inlined: callee too large",
            Self::TooDeep => "inline call not inlined: call inside too many loops",
            Self::Frame => "inline call not inlined: stack frame growth limit reached",
            Self::Unwinds => "inline call not inlined: call has a landing pad",
            Self::Target => "inline call not inlined: target specific option mismatch",
        }
    }

    /// What `-fopt-info` says about it for the one call to a `static` function called once.
    #[must_use]
    pub const fn once(self) -> &'static str {
        match self {
            Self::Recursive => "call to a function called once not inlined: recursive",
            Self::Mismatch => "call to a function called once not inlined: arguments do not match",
            Self::ByValue => {
                "call to a function called once not inlined: structure passed by value"
            }
            Self::VaStart => "call to a function called once not inlined: callee uses va_start",
            Self::ComputedGoto => {
                "call to a function called once not inlined: callee has a computed goto"
            }
            Self::Setjmp => "call to a function called once not inlined: callee calls setjmp",
            Self::ApplyArgs => {
                "call to a function called once not inlined: callee uses __builtin_apply_args"
            }
            Self::MemorySsa => "call to a function called once not inlined: memory SSA present",
            Self::Pack => {
                "call to a function called once not inlined: va_arg_pack cannot be forwarded"
            }
            Self::Alloca => "call to a function called once not inlined: callee calls alloca",
            Self::TooLarge => "call to a function called once not inlined: callee too large",
            Self::TooDeep => {
                "call to a function called once not inlined: call inside too many loops"
            }
            Self::Frame => {
                "call to a function called once not inlined: stack frame growth limit reached"
            }
            Self::Unwinds => "call to a function called once not inlined: call has a landing pad",
            Self::Target => {
                "call to a function called once not inlined: target specific option mismatch"
            }
        }
    }
}

/// Inlines every call to an `always_inline` function that can be, and with a `limit` every call to
/// a function declared `inline` whose body is no larger than that and, when `once` says so, the one
/// call to a `static` function called once, and says what it did where.
///
/// Then turns every function still holding a `va_arg_pack` into a declaration. See the module
/// documentation for why that is the right thing to do with one.
///
/// `isa` is what the module is built for, which is what a function without a `target` attribute
/// is built for. A callee built for more than its caller is never copied into it. `names` is
/// what the names of the functions a body calls are read from, to find a call to `setjmp`.
/// `growth` is how far a caller's frame may grow, which `-fconserve-stack` makes tighter.
pub fn run(
    module: &mut Module,
    names: &Interner,
    limit: Option<u32>,
    once: bool,
    isa: Isa,
    growth: Growth,
    share: bool,
) -> Vec<(FuncId, Stats)> {
    let once = if limit.is_some() && once { called_once(module) } else { Set::default() };
    let wanted: Map<Symbol, (FuncId, Kind)> = module
        .funcs()
        .filter(|&id| !module[id].is_declaration())
        .filter_map(|id| {
            let func = &module[id];
            let set = func.attrs.set;
            let kind = if set.contains(AttrSet::ALWAYS_INLINE) {
                Kind::Always
            } else if limit.is_none()
                || set.without(AttrSet::NOINLINE | AttrSet::OPTNONE | AttrSet::NAKED) != set
            {
                return None;
            } else if func.linkage == Linkage::Internal
                && !set.contains(AttrSet::USED)
                && once.contains(&func.name)
            {
                Kind::Once
            } else if set.contains(AttrSet::INLINE_HINT) {
                Kind::Hinted
            } else {
                return None;
            };
            Some((func.name, (id, kind)))
        })
        .collect();
    let mut done = Vec::new();
    if !wanted.is_empty() {
        let convention = Convention::of(module);
        let mut state = Map::default();
        let limit = limit.map_or(0, |limit| usize::try_from(limit).unwrap_or(usize::MAX));
        let how = How { wanted: &wanted, convention, limit, isa, names, growth, share };
        for id in module.funcs().collect::<Vec<FuncId>>() {
            settle(module, id, &how, &mut state, &mut done);
        }
        let (calls, elsewhere) = references(module);
        for &(id, kind) in wanted.values() {
            let func = &module[id];
            let name = func.name;
            let gone = match kind {
                Kind::Once => true,
                Kind::Always | Kind::Hinted => {
                    func.linkage == Linkage::Internal && !func.attrs.set.contains(AttrSet::USED)
                }
            };
            if gone && !calls.contains_key(&name) && !elsewhere.contains(&name) {
                module[id] = declaration(&module[id]);
            }
        }
        for &(id, _) in &done {
            settle_operands(&mut module[id]);
        }
    }
    withdraw(module);
    done
}

/// Every operand of an assembly statement in that function that is arithmetic over constants,
/// written down as the constant.
///
/// Only an inlined call can leave one, since the front end folds a constant expression it hands
/// to a statement itself. What it cannot fold is `_mm_round_ps (x, _MM_FROUND_TO_NEAREST_INT |
/// _MM_FROUND_NO_EXC)`, where the expression is an argument and the statement is in the callee,
/// and at `-O0` nothing after this folds it either, so an `i` operand would reach the back end
/// as a register. gcc folds it at every level. The instruction that computes the operand becomes
/// the constant where it stands, which is [`crate::fold`]'s rewrite, and so is right for every
/// other use of it too.
fn settle_operands(func: &mut Func) {
    let mut found = Vec::new();
    for block in func.blocks() {
        for inst in func.insts(block) {
            if func[inst].opcode != Opcode::InlineAsm {
                continue;
            }
            for &arg in &func[func[inst].args] {
                let Def::Result { inst: def, .. } = func[arg].def else { continue };
                if matches!(func[def].opcode, Opcode::IConst | Opcode::FConst) {
                    continue;
                }
                if let Some((imm, _)) = crate::fold::evaluated(func, arg, 8) {
                    found.push((def, imm));
                }
            }
        }
    }
    for (def, imm) in found {
        let at = func.add_imm(imm);
        let data = &mut func[def];
        data.opcode = Opcode::IConst;
        data.flags = rucc_ir::Flags::NONE;
        data.args = ValueList::EMPTY;
        data.extra = Extra::Imm(at);
    }
}

/// The names the module reaches by exactly one direct call and in no other way.
///
/// Any other way is a second call, a tail call, an instruction that takes the address, an image
/// that holds it, or an alias that names it. Whether a name is a `static` function that may be
/// inlined is for the caller to ask.
fn called_once(module: &Module) -> Set<Symbol> {
    let (calls, elsewhere) = references(module);
    calls
        .into_iter()
        .filter(|&(name, count)| count == 1 && !elsewhere.contains(&name))
        .map(|(name, _)| name)
        .collect()
}

/// How many direct calls the module makes to each name, and the names it reaches any other way.
fn references(module: &Module) -> (Map<Symbol, usize>, Set<Symbol>) {
    let mut calls: Map<Symbol, usize> = Map::default();
    let mut elsewhere = Set::default();
    for id in module.funcs() {
        let func = &module[id];
        for inst in func.blocks().flat_map(|block| func.insts(block)) {
            match func[inst].extra {
                Extra::Call(info) if func[inst].opcode == Opcode::Call => {
                    if let Some(callee) = func[info].callee {
                        *calls.entry(callee).or_default() += 1;
                    }
                }
                Extra::Call(info) => elsewhere.extend(func[info].callee),
                Extra::Symbol(name) => {
                    elsewhere.insert(name);
                }
                _ => {}
            }
        }
    }
    for id in module.globals() {
        let init = module[id].init.map(|list| &module[list]).unwrap_or_default();
        for datum in init {
            if let Datum::Addr(reloc) | Datum::Away(reloc) | Datum::Apart { to: reloc, .. } = *datum
            {
                elsewhere.insert(module[reloc].symbol);
            }
        }
    }
    for id in module.aliases() {
        elsewhere.insert(module[id].target);
    }
    (calls, elsewhere)
}

/// What stays the same for every function [`settle`] visits.
struct How<'a> {
    /// The functions whose calls are inlined, by name, and why.
    wanted: &'a Map<Symbol, (FuncId, Kind)>,
    /// The calling convention the pack is forwarded under.
    convention: Convention,
    /// How many instructions a callee declared `inline` may have.
    limit: usize,
    /// What a function without a `target` attribute of its own is built for.
    isa: Isa,
    /// What the names in the module are read from.
    names: &'a Interner,
    /// How far a caller's frame may grow.
    growth: Growth,
    /// Whether bodies spliced into the same caller may share their slots. See [`Pool`].
    share: bool,
}

/// How far inlining may grow a caller's frame, gcc's `large-stack-frame-growth` and
/// `large-stack-frame`. See [`fits`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Growth {
    /// How much larger than the caller's own locals its frame may become, in percent.
    pub percent: u32,
    /// The frame size, in bytes, that is never too large.
    pub bytes: u32,
}

impl Growth {
    /// gcc's defaults, 1000 percent and 256 bytes.
    pub const DEFAULT: Self = Self { percent: INLINE_FRAME_GROWTH, bytes: INLINE_LARGE_FRAME };
    /// What gcc sets under `-fconserve-stack`, 40 percent and 100 bytes, which is what the Linux
    /// kernel builds with.
    pub const CONSERVE: Self =
        Self { percent: INLINE_FRAME_GROWTH_CONSERVE, bytes: INLINE_LARGE_FRAME_CONSERVE };
}

/// Where a function is in being settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Its calls are being inlined, so a call back to it from one of them is a cycle.
    Settling,
    /// Every call of this kind in it that can be inlined has been.
    Settled,
}

/// Inlines the `always_inline` calls in one function, settling each callee first.
fn settle(
    module: &mut Module,
    id: FuncId,
    how: &How<'_>,
    state: &mut Map<FuncId, State>,
    done: &mut Vec<(FuncId, Stats)>,
) {
    if state.contains_key(&id) || module[id].is_declaration() {
        return;
    }
    state.insert(id, State::Settling);
    // The caller's own locals, before anything is copied into it, which is what the growth of its
    // frame is measured against.
    let own = frame(&module[id]);
    // The calls as the function was written. A call that arrives inside a body being inlined is
    // one the callee's own settling already had its chance at. A function that asked not to be
    // optimized is left with its calls, except for the ones that are a promise.
    let optnone = module[id].attrs.set.contains(AttrSet::OPTNONE);
    let calls: Vec<(Block, Inst, FuncId, Kind)> = {
        let func = &module[id];
        func.blocks()
            .flat_map(|block| func.insts(block).map(move |inst| (block, inst)))
            .filter_map(|(block, inst)| {
                let Extra::Call(info) = func[inst].extra else { return None };
                if func[inst].opcode != Opcode::Call {
                    return None;
                }
                let callee = func[info].callee?;
                let &(callee, kind) = how.wanted.get(&callee)?;
                (kind == Kind::Always || !optnone).then_some((block, inst, callee, kind))
            })
            .collect()
    };
    // The calls to a function called once that are too many loops deep, found before anything is
    // spliced in, since a splice splits the block the call was in. Counted as gcc counts, so a
    // block in one loop is one deep, which is one more than `Loops::depth` says.
    let deep: Set<Inst> = if calls.iter().any(|&(.., kind)| kind == Kind::Once) {
        let func = &module[id];
        let cfg = Cfg::new(func);
        let loops = Loops::new(&cfg, &Dominators::new(&cfg));
        let depth = |block| loops.innermost(block).map_or(0, |inner| loops.depth(inner) + 1);
        calls
            .iter()
            .filter(|&&(block, _, _, kind)| {
                kind == Kind::Once && depth(block) > INLINE_CALLED_ONCE_LOOP_DEPTH
            })
            .map(|&(_, inst, ..)| inst)
            .collect()
    } else {
        Set::default()
    };
    let mut stats = Stats::new();
    let mut spliced = false;
    let mut pool = Pool { on: how.share, ..Pool::default() };
    for (_, call, callee, kind) in calls {
        let why = |failure: InlineFailure| match kind {
            Kind::Always => failure.why(),
            Kind::Hinted => failure.hint(),
            Kind::Once => failure.once(),
        };
        if callee == id || state.get(&callee) == Some(&State::Settling) {
            stats.missed(why(InlineFailure::Recursive));
            continue;
        }
        if deep.contains(&call) {
            stats.missed(why(InlineFailure::TooDeep));
            continue;
        }
        // A body built for more than the caller cannot go into it, whatever else is true of the
        // call, so this is asked before anything is settled or measured.
        if let Some(wanted) = module[callee].target {
            if !module[id].target.unwrap_or(how.isa).covers(wanted) {
                stats.missed(why(InlineFailure::Target));
                continue;
            }
        }
        settle(module, callee, how, state, done);
        // A body that calls `sigsetjmp` would take its `jmp_buf` into the caller's frame on every
        // path, fast ones too, and make the caller a function that comes back twice, so it stays a
        // call whatever kind it is. That is `__builtin_setjmp` in `check` by another road, and gcc
        // refuses both, saying the function can never be inlined because it uses setjmp.
        if calls_twice(module, &module[callee], how.names) {
            stats.missed(why(InlineFailure::Setjmp));
            continue;
        }
        // Measured once the callee is settled, since what is copied is the body with its own
        // calls already inlined.
        let most = match kind {
            Kind::Always => usize::MAX,
            Kind::Hinted => how.limit,
            Kind::Once => INLINE_CALLED_ONCE_INSNS as usize,
        };
        if size(&module[callee]) > most {
            stats.missed(why(InlineFailure::TooLarge));
            continue;
        }
        if kind != Kind::Always
            && !fits(own, frame(&module[id]), pool.growth(&module[callee]), how.growth)
        {
            stats.missed(why(InlineFailure::Frame));
            continue;
        }
        match splice(module, id, call, callee, how.convention, kind, &mut pool) {
            Ok(()) => {
                spliced = true;
                stats.optimized(match kind {
                    Kind::Always => INLINED,
                    Kind::Hinted => HINT_INLINED,
                    Kind::Once => ONCE_INLINED,
                });
            }
            Err(failure) => stats.missed(why(failure)),
        }
    }
    // A body that never returns ends in something other than a jump back, so the block the call
    // was in stops before the rest of the caller and nothing reaches the rest any more. The pass
    // that strands a block deletes it, which is what the verifier holds every pass to, and doing it
    // here rather than at the end means a caller that copies this body in does not copy the dead
    // blocks along with it. What it removes is not a decision of this pass and is not counted.
    if spliced {
        let mut an = crate::Analyses::new(crate::machine::Machine::unknown());
        crate::simplify_cfg::sweep(&mut module[id], &mut an, &mut Stats::new());
    }
    state.insert(id, State::Settled);
    if !stats.is_empty() {
        done.push((id, stats));
    }
}

/// Whether a body calls something that comes back more than once, by the rule the code generator
/// uses to lay out the frame of a function that does, `rucc_ir::Module::returns_twice`.
fn calls_twice(module: &Module, func: &Func, names: &Interner) -> bool {
    func.blocks().any(|block| {
        func.insts(block).any(|inst| {
            let Extra::Call(info) = func[inst].extra else { return false };
            func[inst].opcode == Opcode::Call
                && func[info].callee.is_some_and(|callee| module.returns_twice(callee, names))
        })
    })
}

/// The slots the bodies spliced into one caller brought with them, which the bodies spliced in
/// after them may take over.
///
/// A body's locals last as long as the call it stands for, and the calls a function was written
/// with are made one after another, never one inside another, so the locals of two of them are
/// never wanted at once. gcc gives them the same bytes, which is how a function that calls four
/// `always_inline` helpers with a buffer each has one buffer in its frame and not four, and the
/// kernel is sized on that. What is pooled is only what a splice brought. The caller's own locals
/// may be wanted across every call it makes, and a body inside a body already spliced came in with
/// that body and was pooled or not there, in the callee.
///
/// Only slots of the same size are shared, for the reason the lowering gives for its own sharing:
/// the size of an `alloca` is the size every later pass reads as the object's, and
/// `__builtin_object_size` would answer for the smaller with the larger. One slot is taken at most
/// once by each splice, since two locals of one body may well be wanted at once.
#[derive(Debug, Default)]
struct Pool {
    /// Whether anything is shared at all, which is `-fstack-reuse=` and off at `-O0`.
    on: bool,
    /// Which splice this is, counting from one.
    site: u32,
    /// The slots so far, each with its size and the last splice that took it.
    slots: Vec<(Inst, u64, u32)>,
}

impl Pool {
    /// A slot for the callee's `alloca` whose memory is `extra` to take over, when there is one.
    fn take(&mut self, func: &mut Func, callee: &Func, extra: Extra) -> Option<Value> {
        let Extra::Mem(mem) = extra else { return None };
        if !self.on {
            return None;
        }
        let wanted = callee[mem];
        let site = self.site;
        let (inst, _, taken) = self
            .slots
            .iter_mut()
            .find(|&&mut (_, size, taken)| size == wanted.size && taken != site)?;
        *taken = site;
        let Extra::Mem(held) = func[*inst].extra else { return None };
        func.align_mem(held, wanted.align);
        func[*inst].first_result
    }

    /// How many bytes splicing `callee` in would add to the caller's frame, which is its [`frame`]
    /// less the slots it would take over.
    fn growth(&self, callee: &Func) -> u64 {
        let mut free: Vec<u64> = if self.on {
            self.slots.iter().map(|&(_, size, _)| size).collect()
        } else {
            Vec::new()
        };
        let mut grows = 0;
        for inst in callee.blocks().flat_map(|block| callee.insts(block)) {
            if callee[inst].opcode != Opcode::Alloca || !callee[inst].args.is_empty() {
                continue;
            }
            let Extra::Mem(mem) = callee[inst].extra else { continue };
            let size = callee[mem].size;
            match free.iter().position(|&held| held == size) {
                Some(at) => {
                    free.swap_remove(at);
                }
                None => grows += size,
            }
        }
        grows
    }

    /// Puts the `alloca` just copied in as `inst` on the list, for a later splice to take.
    fn add(&mut self, func: &Func, inst: Inst, callee: &Func, extra: Extra) {
        let Extra::Mem(mem) = extra else { return };
        if self.on && func[inst].first_result.is_some() {
            self.slots.push((inst, callee[mem].size, self.site));
        }
    }
}

/// How many instructions a body has, which is what the limit on a callee declared `inline` counts.
fn size(func: &Func) -> usize {
    func.blocks().map(|block| func.insts(block).count()).sum()
}

/// How many bytes of locals a body keeps in memory, which is every `alloca` of a fixed size in it.
///
/// The lowering already gave every local that never has its address taken a register, so what is
/// left is the arrays, the structures and the scalars something points at. That is gcc's
/// `estimated_stack_size`. The locals of blocks that never overlap are already one slot by the
/// time this counts them, since the lowering shares them, and so are the slots of bodies spliced
/// in earlier, see [`Pool`], so the sum is close to what the frame will be.
fn frame(func: &Func) -> u64 {
    func.blocks()
        .flat_map(|block| func.insts(block))
        .filter(|&inst| func[inst].opcode == Opcode::Alloca && func[inst].args.is_empty())
        .filter_map(|inst| match func[inst].extra {
            Extra::Mem(mem) => Some(func[mem].size),
            _ => None,
        })
        .sum()
}

/// Whether a caller whose own locals came to `own` bytes, and whose locals come to `now` bytes with
/// what has been inlined into it so far, can take a body whose locals come to `body` more, when the
/// frame may grow as far as `growth` says.
///
/// This is gcc's `caller_growth_limits` test for the stack. The frame may grow to
/// `large-stack-frame-growth` percent more than the caller's own locals, and a frame no larger than
/// `large-stack-frame` bytes is always fine. Those are 1000 and 256 by default and 40 and 100 under
/// `-fconserve-stack`. gcc also lets a call through when a sibling already made the frame that
/// large, on the grounds that the two bodies will share bytes. Here a body whose slots all fit in
/// ones a sibling brought adds nothing to [`frame`], since it takes those over, but one that does
/// not is still measured on its own, which is less than gcc lets through.
///
/// Without this a small function with a large buffer, called once from a function with none,
/// moves the buffer into the caller, and a caller of many such helpers ends up with all their
/// buffers at once where gcc has one at a time. `select_default_timezone` in postgres's `initdb`
/// had a frame of 44560 bytes this way, against gcc's 16.
fn fits(own: u64, now: u64, body: u64, growth: Growth) -> bool {
    let limit = own + own * u64::from(growth.percent) / 100;
    let after = now + body;
    after <= limit || after <= u64::from(growth.bytes)
}

/// Inlines one call, or says why not and leaves the caller as it was.
fn splice(
    module: &mut Module,
    caller: FuncId,
    call: Inst,
    callee: FuncId,
    convention: Convention,
    kind: Kind,
    pool: &mut Pool,
) -> Result<(), InlineFailure> {
    // Out of the module for the length of the splice, so that the callee can be read while the
    // caller is written. The two are different functions, since a call to itself is refused
    // before this.
    let stand_in = Func::new(module[caller].name, Signature::new());
    let mut func = std::mem::replace(&mut module[caller], stand_in);
    let result = check(&func, call, &module[callee], convention, kind)
        .map(|plan| copy(&mut func, call, &module[callee], &plan, pool));
    module[caller] = func;
    result
}

/// What [`check`] found out that [`copy`] needs.
struct Plan {
    /// How many of the call's arguments go to the callee's entry block.
    fixed: usize,
    /// The rest of them, which are what a `va_arg_pack` stands for.
    extras: Vec<Value>,
    /// How each of those travels, one for each.
    abis: Vec<Abi>,
    /// How many of them each C argument became, where the lowering said.
    groups: Option<Vec<u32>>,
    /// For each call in the callee that passes the pack on, the groups that have to go to memory
    /// because the registers they went in are taken there, which is only ever under SysV.
    spills: Map<Inst, Vec<usize>>,
}

/// Whether one call can be inlined, and what the splice needs to know if it can.
fn check(
    func: &Func,
    call: Inst,
    callee: &Func,
    convention: Convention,
    kind: Kind,
) -> Result<Plan, InlineFailure> {
    // The pad is for an unwind out of this call, and the table that says so names the call by
    // where it is. A copy of the body in its place is calls the table knows nothing about, so
    // `copy` gives each of them the same pad, which needs the pad to be found.
    if func.unwinds_to_pad(call) && unwind_arms(func, call).is_none() {
        return Err(InlineFailure::Unwinds);
    }
    let entry = callee.entry().ok_or(InlineFailure::Mismatch)?;
    let params = &callee[entry].params;
    let args = &func[func[call].args];
    let Extra::Call(info) = func[call].extra else { return Err(InlineFailure::Mismatch) };
    let signature = &func[func[info].signature];
    if args.len() < params.len()
        || (args.len() > params.len() && !callee.signature().variadic)
        || args.iter().zip(params).any(|(&arg, &param)| func[arg].ty != callee[param].ty)
    {
        return Err(InlineFailure::Mismatch);
    }
    let returns: Vec<Type> = callee.signature().return_types().collect();
    let results: Vec<Type> = func[call].results().map(|value| func[value].ty).collect();
    if results.len() > returns.len() || results.iter().zip(&returns).any(|(a, b)| a != b) {
        return Err(InlineFailure::Mismatch);
    }
    // An `always_inline` function that takes a structure or a vector by value is inlined with a
    // copy of the argument in the caller, which is what the call would have made. The intrinsics
    // over 64 byte vectors are all of this kind, and gcc inlines them at every level. Other calls
    // are still left alone, since that changes which calls are inlined across a whole program.
    if kind != Kind::Always
        && callee.signature().params.iter().any(|param| matches!(param.abi, Abi::ByVal { .. }))
    {
        return Err(InlineFailure::ByValue);
    }

    let fixed = params.len();
    let extras = args[fixed..].to_vec();
    let abis = expand(&func[func[info].varargs], extras.len());
    let groups = func.arg_groups(call).and_then(|groups| past(groups, fixed));
    let mut plan = Plan { fixed, extras, abis, groups, spills: Map::default() };
    let outer: Vec<(Type, Abi)> = args[..fixed]
        .iter()
        .enumerate()
        .map(|(at, &arg)| (func[arg].ty, signature.params.get(at).map_or(Abi::Plain, |p| p.abi)))
        .collect();

    if callee.named_blocks().next().is_some() {
        return Err(InlineFailure::ComputedGoto);
    }
    let mut packs = Set::default();
    let mut counted = false;
    for block in callee.blocks() {
        for inst in callee.insts(block) {
            match callee[inst].opcode {
                Opcode::VaStart => return Err(InlineFailure::VaStart),
                Opcode::IndirectBr => return Err(InlineFailure::ComputedGoto),
                Opcode::Alloca if kind != Kind::Always && !callee[inst].args.is_empty() => {
                    return Err(InlineFailure::Alloca);
                }
                Opcode::SetjmpMarker => return Err(InlineFailure::Setjmp),
                Opcode::ApplyArgs => return Err(InlineFailure::ApplyArgs),
                Opcode::MemEntry => return Err(InlineFailure::MemorySsa),
                Opcode::VaArgPack => packs.extend(callee[inst].results()),
                Opcode::VaArgPackLen => counted = true,
                _ => {}
            }
        }
    }
    // A pack standing for another pack is the caller being an inline definition itself, and
    // what that pack stands for, or how many it is, is not known until the caller is inlined
    // somewhere.
    if (counted || !packs.is_empty()) && plan.extras.iter().any(|&value| is_pack(func, value)) {
        return Err(InlineFailure::Pack);
    }
    if packs.is_empty() {
        return Ok(plan);
    }
    for block in callee.blocks() {
        for inst in callee.insts(block) {
            let data = &callee[inst];
            let used = callee[data.args].iter().position(|value| packs.contains(value));
            let passed = callee
                .successors(inst)
                .any(|to| callee[to.args].iter().any(|value| packs.contains(value)));
            if passed {
                return Err(InlineFailure::Pack);
            }
            let Some(at) = used else { continue };
            let args = &callee[data.args];
            let Extra::Call(inner) = data.extra else { return Err(InlineFailure::Pack) };
            if at + 1 != args.len() || !matches!(data.opcode, Opcode::Call | Opcode::CallIndirect) {
                return Err(InlineFailure::Pack);
            }
            let skip = usize::from(data.opcode == Opcode::CallIndirect);
            let named = &callee[callee[inner].signature].params;
            let written = &args[skip..at];
            let anonymous = expand(&callee[callee[inner].varargs], written.len() + 1 - named.len());
            let before: Vec<(Type, Abi)> = written
                .iter()
                .enumerate()
                .map(|(index, &value)| {
                    let abi = match named.get(index) {
                        Some(param) => param.abi,
                        None => anonymous[index - named.len()],
                    };
                    (callee[value].ty, abi)
                })
                .collect();
            let forwarded: Vec<(Type, Abi)> = plan
                .extras
                .iter()
                .zip(&plan.abis)
                .map(|(&value, &abi)| (func[value].ty, abi))
                .collect();
            let spills =
                forwardable(convention, &outer, &before, &forwarded, plan.groups.as_deref())
                    .ok_or(InlineFailure::Pack)?;
            if !spills.is_empty() {
                plan.spills.insert(inst, spills);
            }
        }
    }
    Ok(plan)
}

/// Whether a value is what a `va_arg_pack` produced.
fn is_pack(func: &Func, value: Value) -> bool {
    matches!(func[value].def, Def::Result { inst, .. } if func[inst].opcode == Opcode::VaArgPack)
}

/// A list of how the anonymous arguments travel, with the empty one that means every one of them
/// is plain written out.
fn expand(abis: &[Abi], count: usize) -> Vec<Abi> {
    if abis.is_empty() { vec![Abi::Plain; count] } else { abis.to_vec() }
}

/// The groups past the first `fixed` values, or `None` when a group straddles that point, which
/// no lowering does.
fn past(groups: &[u32], fixed: usize) -> Option<Vec<u32>> {
    let mut seen = 0;
    let mut rest = groups.iter();
    while seen < fixed {
        seen += usize::try_from(*rest.next()?).ok()?;
    }
    (seen == fixed).then(|| rest.copied().collect())
}

/// The calling convention, as far as forwarding arguments from one call to another cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Convention {
    /// x86-64 System V, where a structure goes in registers whole or not at all.
    SysV,
    /// Windows x64, where every argument is one slot and nothing depends on what came before.
    Slots,
    /// Everything else, where the answer is only trusted when nothing moves.
    Other,
}

impl Convention {
    /// Read off the ABI description the target's registers are the other half of, so that a
    /// target that takes one of these two conventions gets the answer without being named here.
    fn of(module: &Module) -> Self {
        match TargetInfo::for_tuple(module.tuple).call_regs.map(|regs| regs.abi) {
            Some(abi) if std::ptr::eq(abi, &rucc_abi::abis::WIN64) => Self::Slots,
            Some(abi) if std::ptr::eq(abi, &rucc_abi::abis::SYSV_AMD64) => Self::SysV,
            _ => Self::Other,
        }
    }
}

/// Where one value goes under SysV, as far as registers are concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// General purpose registers, this many of them.
    Gpr(u32),
    /// One vector register.
    Sse,
    /// The argument area, whatever registers are left.
    Memory,
}

fn class(ty: Type, abi: Abi) -> Class {
    if abi.indirect() && !matches!(abi, Abi::Sret { .. }) {
        Class::Memory
    } else if ty.is_vector() {
        Class::Sse
    } else if ty.is_float() {
        if ty.format() == Some(Float::F80) { Class::Memory } else { Class::Sse }
    } else if ty.is_int() && ty.bits() > 64 {
        Class::Gpr(2)
    } else {
        Class::Gpr(1)
    }
}

/// The registers a SysV call has used so far.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Regs {
    gpr: u32,
    sse: u32,
}

impl Regs {
    const GPR: u32 = 6;
    const SSE: u32 = 8;

    fn after(values: &[(Type, Abi)]) -> Self {
        let mut regs = Self::default();
        for &(ty, abi) in values {
            regs.take(class(ty, abi));
        }
        regs
    }

    fn fits(self, gpr: u32, sse: u32) -> bool {
        self.gpr + gpr <= Self::GPR && self.sse + sse <= Self::SSE
    }

    /// Takes what one value of that class needs, if there is room, and says whether there was.
    fn take(&mut self, class: Class) -> bool {
        let (gpr, sse) = match class {
            Class::Gpr(count) => (count, 0),
            Class::Sse => (0, 1),
            Class::Memory => return false,
        };
        let room = self.fits(gpr, sse);
        if room {
            self.gpr += gpr;
            self.sse += sse;
        }
        room
    }
}

/// Whether the anonymous arguments of one call can be passed on to another, and which of them
/// have to go to memory on the way.
///
/// `outer` is what the first call passes to the named parameters, `before` is what the second
/// passes ahead of the pack, and `forwarded` is what the pack stands for, all as the lowering left
/// them. The answer is yes when the second call has used the same registers as the first by the
/// time the pack starts, since then every argument goes where it went. Under SysV it is also yes
/// when the registers used differ but no argument that decides where it goes as a whole would
/// decide differently, which is a small structure in memory that would now fit. A structure in
/// registers that no longer fits goes to memory, as it would have if the second call had been
/// written out, and the answer says which groups those are. `groups` is what says which values
/// are one structure, and without it only the first answer is given.
fn forwardable(
    convention: Convention,
    outer: &[(Type, Abi)],
    before: &[(Type, Abi)],
    forwarded: &[(Type, Abi)],
    groups: Option<&[u32]>,
) -> Option<Vec<usize>> {
    match convention {
        Convention::Slots => Some(Vec::new()),
        Convention::Other => {
            let count = |values: &[(Type, Abi)]| {
                let mut ints = 0;
                let mut floats = 0;
                for &(ty, abi) in values {
                    if abi.indirect() {
                        return None;
                    }
                    if ty.is_float() || ty.is_vector() { floats += 1 } else { ints += 1 }
                }
                Some((ints, floats))
            };
            (count(outer).is_some() && count(outer) == count(before)).then(Vec::new)
        }
        Convention::SysV => {
            let mut first = Regs::after(outer);
            let mut second = Regs::after(before);
            if first == second {
                return Some(Vec::new());
            }
            let mut spills = Vec::new();
            let mut at = 0;
            for (index, &count) in groups?.iter().enumerate() {
                let group = usize::try_from(count).ok().and_then(|n| forwarded.get(at..at + n))?;
                at += group.len();
                match *group {
                    [] => {}
                    // One value, which goes wherever the registers left send it in either call,
                    // except for a small structure in memory. That may be there because it did
                    // not fit, and if the second call has more room it would be in registers.
                    [(ty, abi)] => {
                        if let Abi::ByVal { size, .. } = abi {
                            let small = size <= 16; // not a threshold: the SysV register limit
                            if small && (second.gpr < first.gpr || second.sse < first.sse) {
                                return None;
                            }
                            continue;
                        }
                        first.take(class(ty, abi));
                        second.take(class(ty, abi));
                    }
                    // A structure in registers, which the first call found room for. It goes in
                    // the second one's registers if there is room, and to memory if not.
                    _ => {
                        let mut gpr = 0;
                        let mut sse = 0;
                        for &(ty, abi) in group {
                            match class(ty, abi) {
                                Class::Gpr(count) => gpr += count,
                                Class::Sse => sse += 1,
                                Class::Memory => return None,
                            }
                        }
                        if !first.fits(gpr, sse) {
                            return None;
                        }
                        first.gpr += gpr;
                        first.sse += sse;
                        if second.fits(gpr, sse) {
                            second.gpr += gpr;
                            second.sse += sse;
                        } else {
                            spills.push(index);
                        }
                    }
                }
            }
            (at == forwarded.len()).then_some(spills)
        }
    }
}

/// Splices the callee in where the call is, which [`check`] has said it can be.
fn copy(func: &mut Func, call: Inst, callee: &Func, plan: &Plan, pool: &mut Pool) {
    let block = func.block_of(call).expect("a call being inlined is in a block");
    let entry = func.entry().expect("a function with a call in it has a body");
    // Where an unwind out of the call went, and where a return from it went, when a `cleanup`
    // handler's scope gave it a pad. Read before the block is split, since the split moves the
    // branch that says so.
    let arms = unwind_arms(func, call);

    // The part after the call, which takes the call's results as parameters.
    let after = func.create_block();
    let mut forward = Map::default();
    for result in func[call].results().collect::<Vec<Value>>() {
        let ty = func[result].ty;
        forward.insert(result, func.append_param(after, ty));
    }
    let moving: Vec<Inst> = func.insts(block).skip_while(|&inst| inst != call).skip(1).collect();
    for inst in moving {
        func.remove_inst(inst);
        func.append_inst(after, inst);
    }
    // With the call gone the `unwound` behind it has nothing to ask about, and the branch on it
    // goes where a return went, always.
    if let Some((unwound, branch, _, returned)) = arms {
        func.remove_inst(unwound);
        func.remove_inst(branch);
        let args = func[returned.args].to_vec();
        let args = func.push_values(&args);
        let to = func.push_block_calls(&[BlockCall { args, ..returned }]);
        let span = func.span(branch);
        let jump = func.create_inst(
            InstData { extra: Extra::Targets(to), ..InstData::new(Opcode::Jump) },
            &[],
            span,
        );
        func.append_inst(after, jump);
    }

    // The callee's blocks and their parameters, and then its instructions with their results, so
    // that every value exists before any operand is written.
    //
    // The entry block's parameters are the call's arguments themselves rather than parameters of
    // the copy, since nothing branches to an entry block and so nothing else arrives there. That
    // way a constant argument is a constant in the body straight away, and the folding that runs
    // next sees `1 + 1` rather than a block parameter that only `simplify-cfg` would later find
    // is always `1`.
    let start = callee.entry().expect("checked to have a body");
    let mut passed = func[func[call].args][..plan.fixed].to_vec();
    // An argument passed by value is the callee's own copy, which it may write to, so the body
    // gets a copy made in the caller's frame just before the call, as the call would have.
    for (at, param) in callee.signature().params.iter().enumerate().take(plan.fixed) {
        if let Abi::ByVal { size, align, .. } = param.abi {
            passed[at] = by_value(func, entry, call, passed[at], size, align);
        }
    }
    let mut blocks = Map::default();
    let mut values = Map::default();
    for from in callee.blocks() {
        let to = func.create_block();
        if from == start {
            values.extend(callee[from].params.iter().copied().zip(passed.iter().copied()));
        } else {
            for &param in &callee[from].params {
                values.insert(param, func.append_param(to, callee[param].ty));
            }
        }
        blocks.insert(from, to);
    }
    // The lowering gives the callee's slots and the stores of its parameters the span of its
    // whole body, which the line table reads as the line of the opening brace. That is the right
    // answer in the callee's own prologue and the wrong one here: that line is outside the caller,
    // and a line table that names it inside the caller sends a debugger to the top of another
    // function. What those instructions do in the caller is take the arguments of the call, so
    // they say the call instead, which is what gcc says for them. Every statement in the callee
    // keeps its own place, and a callee with no body span, which is one the IR parser built, keeps
    // every span it has.
    let at = func.span(call);
    let body = callee.declared;
    let spliced = |inst: Inst| {
        let span = callee.span(inst);
        let prologue = span == body || !body.contains(span.lo);
        if span.is_dummy() || body.is_dummy() || !prologue { span } else { at }
    };
    let mut made = Vec::new();
    pool.site += 1;
    for from in callee.blocks() {
        for inst in callee.insts(from) {
            let data = &callee[inst];
            if data.opcode == Opcode::VaArgPack {
                continue;
            }
            let opcode = match data.opcode {
                Opcode::Return => Opcode::Jump,
                Opcode::VaArgPackLen => Opcode::IConst,
                opcode => opcode,
            };
            let types: Vec<Type> = data.results().map(|value| callee[value].ty).collect();
            let shell = InstData { flags: data.flags, ..InstData::new(opcode) };
            let fixed = opcode == Opcode::Alloca && data.args.is_empty();
            let taken = if fixed { pool.take(func, callee, data.extra) } else { None };
            if let Some(slot) = taken {
                let old = data.first_result.expect("an alloca has an address");
                values.insert(old, slot);
                continue;
            }
            let first = func.insts(entry).next().expect("an entry block ends in something");
            let span = if fixed {
                // A slot joins the caller's frame, so it says what the caller's own slots say.
                func.span(first)
            } else {
                spliced(inst)
            };
            let new = func.create_inst(shell, &types, span);
            for (old, value) in data.results().zip(func[new].results().collect::<Vec<Value>>()) {
                values.insert(old, value);
            }
            if fixed {
                func.insert_before(new, first);
                pool.add(func, new, callee, data.extra);
            } else {
                func.append_inst(blocks[&from], new);
            }
            made.push((inst, new));
        }
    }

    // The calls of the body that an unwind leaves with no pad of the callee's to run, which are
    // the ones given the caller's below. A call with a pad of its own already goes somewhere, and
    // that pad ends in `_Unwind_Resume`, which is one of these.
    let bare: Vec<Inst> = made
        .iter()
        .filter(|&&(inst, _)| {
            matches!(callee[inst].opcode, Opcode::Call | Opcode::CallIndirect)
                && !callee.unwinds_to_pad(inst)
        })
        .map(|&(_, new)| new)
        .collect();

    let keep = func[call].results().count();
    for (inst, new) in made {
        let data = &callee[inst];
        let mut args: Vec<Value> = callee[data.args]
            .iter()
            .filter(|value| values.contains_key(value))
            .map(|value| values[value])
            .collect();
        let packed = args.len() != data.args.len();
        let extra = if data.opcode == Opcode::Return {
            args.truncate(keep);
            let to = func.push_values(&args);
            args.clear();
            Extra::Targets(func.push_block_calls(&[BlockCall::new(after, to)]))
        } else if data.opcode == Opcode::VaArgPackLen {
            // How many C arguments the pack stands for, which is the groups where the lowering
            // said and one value each where it did not.
            let count = plan.groups.as_ref().map_or(plan.extras.len(), Vec::len);
            let count = i128::try_from(count).expect("fewer arguments than that");
            Extra::Imm(func.add_imm(Imm::int(count, Type::int(32))))
        } else {
            match data.extra {
                Extra::Imm(imm) => Extra::Imm(func.add_imm(callee[imm])),
                Extra::Mem(mem) => Extra::Mem(func.add_mem(unscoped(callee[mem]))),
                Extra::Rmw(op, mem) => Extra::Rmw(op, func.add_mem(unscoped(callee[mem]))),
                Extra::Targets(list) => {
                    Extra::Targets(targets(func, callee, list, &blocks, &values))
                }
                Extra::Call(info) => {
                    let info = callee[info];
                    let mut forwarded = None;
                    let signature = callee[info.signature].clone();
                    let mut abis = callee[info.varargs].to_vec();
                    if packed {
                        let skip = usize::from(data.opcode == Opcode::CallIndirect);
                        let written = args.len() - skip - signature.params.len();
                        abis = expand(&abis, written + 1);
                        abis.truncate(written);
                        let spills = plan.spills.get(&inst).map_or(&[][..], Vec::as_slice);
                        forwarded = pass_on(func, entry, new, spills, plan, &mut args, &mut abis);
                        if abis.iter().all(|&abi| abi == Abi::Plain) {
                            abis.clear();
                        }
                    }
                    let signature = func.add_signature(signature);
                    let varargs = func.push_abis(&abis);
                    if let Some(groups) = callee.arg_groups(inst) {
                        let mut groups = groups.to_vec();
                        let known = if packed {
                            groups.pop();
                            forwarded.as_ref().map(|outer| groups.extend_from_slice(outer))
                        } else {
                            Some(())
                        };
                        if known.is_some() {
                            func.set_arg_groups(new, groups);
                        }
                    }
                    Extra::Call(func.add_call(CallInfo { callee: info.callee, signature, varargs }))
                }
                Extra::Switch(info) => {
                    let info = callee[info];
                    let cases = func.push_imms(&callee[info.cases]);
                    let targets = targets(func, callee, info.targets, &blocks, &values);
                    Extra::Switch(func.add_switch(SwitchInfo { targets, cases }))
                }
                Extra::Asm(info) => {
                    let info = callee[info];
                    let targets = targets(func, callee, info.targets, &blocks, &values);
                    Extra::Asm(func.add_asm(AsmInfo { targets, ..info }))
                }
                Extra::VaObject(info) => {
                    let info = callee[info];
                    let mem = func.add_mem(unscoped(callee[info.mem]));
                    let slots = func.push_slots(&callee[info.slots]);
                    Extra::VaObject(func.add_va_object(VaInfo { mem, slots }))
                }
                other => other,
            }
        };
        func[new].args = if args.is_empty() { ValueList::EMPTY } else { func.push_values(&args) };
        func[new].extra = extra;
    }

    // And the call itself, which becomes a jump to the copy of the entry block.
    let to = ValueList::EMPTY;
    let targets = func.push_block_calls(&[BlockCall::new(blocks[&start], to)]);
    let span = func.span(call);
    let jump = func.create_inst(
        InstData { extra: Extra::Targets(targets), ..InstData::new(Opcode::Jump) },
        &[],
        span,
    );
    crate::uses::substitute(func, &forward);
    func.remove_inst(call);
    func.append_inst(block, jump);

    // An unwind out of any of those calls passes through the call that was inlined, so it owes
    // what that call's pad does. Each one gets the edge the lowering gives a call in a handler's
    // scope, an `unwound` and a branch on it to the pad, with the rest of its block moved behind
    // the branch. Several calls sharing one pad is fine, since the code generator finds a call's
    // pad by its branch and no machine edge ever enters one.
    if let Some((_, _, pad, _)) = arms {
        for new in bare {
            let block = func.block_of(new).expect("a copied call is in a block");
            let rest = func.create_block();
            let moving: Vec<Inst> =
                func.insts(block).skip_while(|&inst| inst != new).skip(1).collect();
            for inst in moving {
                func.remove_inst(inst);
                func.append_inst(rest, inst);
            }
            let span = func.span(new);
            let unwound = func.create_inst(InstData::new(Opcode::Unwound), &[Type::I1], span);
            func.append_inst(block, unwound);
            let cond = func[unwound].results().next().expect("an unwound has its answer");
            let args = func[pad.args].to_vec();
            let args = func.push_values(&args);
            let to = func.push_block_calls(&[
                BlockCall { args, ..pad },
                BlockCall::new(rest, ValueList::EMPTY),
            ]);
            let branch = func.create_inst(
                InstData { extra: Extra::Targets(to), ..InstData::new(Opcode::BrIf) },
                &[],
                span,
            );
            func[branch].args = func.push_values(&[cond]);
            func.append_inst(block, branch);
        }
    }
}

/// The `unwound` behind a call with a pad, the branch on it that ends the call's block, and the
/// branch's two arms, the pad first and then where a return goes. `None` for a call with no pad,
/// and for one whose edge is not in the shape the lowering builds.
fn unwind_arms(func: &Func, call: Inst) -> Option<(Inst, Inst, BlockCall, BlockCall)> {
    let unwound = func.next_inst(call).filter(|&next| func[next].opcode == Opcode::Unwound)?;
    let block = func.block_of(call)?;
    let branch = func.insts(block).last()?;
    let answer = func[unwound].results().next()?;
    if func[branch].opcode != Opcode::BrIf || func[func[branch].args].first() != Some(&answer) {
        return None;
    }
    let Extra::Targets(list) = func[branch].extra else { return None };
    match func[list] {
        [pad, returned] => Some((unwound, branch, pad, returned)),
        _ => None,
    }
}

/// Appends what the pack stands for to the arguments of one call, putting each group the plan
/// says has to go to memory in a slot of the caller's that the call copies from, and gives back
/// the groups as they are after that.
fn pass_on(
    func: &mut Func,
    entry: Block,
    call: Inst,
    spills: &[usize],
    plan: &Plan,
    args: &mut Vec<Value>,
    abis: &mut Vec<Abi>,
) -> Option<Vec<u32>> {
    let Some(groups) = plan.groups.as_deref().filter(|_| !spills.is_empty()) else {
        args.extend_from_slice(&plan.extras);
        abis.extend_from_slice(&plan.abis);
        return plan.groups.clone();
    };
    let mut now = Vec::with_capacity(groups.len());
    let mut at = 0;
    for (index, &count) in groups.iter().enumerate() {
        let end = at + count as usize;
        if spills.contains(&index) {
            let (slot, size) = spill(func, entry, call, &plan.extras[at..end]);
            args.push(slot);
            abis.push(Abi::ByVal { size, align: 8, drains: Drains::Nothing });
            now.push(1);
        } else {
            args.extend_from_slice(&plan.extras[at..end]);
            abis.extend_from_slice(&plan.abis[at..end]);
            now.push(count);
        }
        at = end;
    }
    Some(now)
}

/// A slot of `size` bytes in the caller's frame with the object `from` points at copied into it
/// just before the call, which is what a call passing that object by value makes.
fn by_value(
    func: &mut Func,
    entry: Block,
    call: Inst,
    from: Value,
    size: u64,
    align: u32,
) -> Value {
    let span = func.span(call);
    let info = MemInfo {
        size,
        align,
        order: MemOrder::NotAtomic,
        tbaa: None,
        owns: 0,
        restrict: Restrict::NONE,
    };
    let mem = func.add_mem(info);
    let alloca = InstData { extra: Extra::Mem(mem), ..InstData::new(Opcode::Alloca) };
    let alloca = func.create_inst(alloca, &[Type::PTR], span);
    let first = func.insts(entry).next().expect("an entry block ends in something");
    func.insert_before(alloca, first);
    let slot = func[alloca].results().next().expect("an alloca has a result");
    let copy = InstData {
        args: func.push_values(&[slot, from]),
        extra: Extra::Mem(func.add_mem(info)),
        ..InstData::new(Opcode::Memcpy)
    };
    let copy = func.create_inst(copy, &[], span);
    func.insert_before(copy, call);
    slot
}

/// Stores the pieces of one structure, eight bytes apart the way the registers held them, in a
/// new slot at the top of the caller, just ahead of the call, and gives back the slot and its size.
fn spill(func: &mut Func, entry: Block, call: Inst, pieces: &[Value]) -> (Value, u64) {
    let span = func.span(call);
    let bytes = |ty: Type| {
        if ty == Type::PTR { 8 } else { u64::from(ty.bits() * ty.lanes()).div_ceil(8) }
    };
    let size: u64 = pieces.iter().map(|&piece| bytes(func[piece].ty).next_multiple_of(8)).sum();
    let info = MemInfo {
        size,
        align: 8,
        order: MemOrder::NotAtomic,
        tbaa: None,
        owns: 0,
        restrict: Restrict::NONE,
    };
    let mem = func.add_mem(info);
    let alloca = InstData { extra: Extra::Mem(mem), ..InstData::new(Opcode::Alloca) };
    let alloca = func.create_inst(alloca, &[Type::PTR], span);
    let first = func.insts(entry).next().expect("an entry block ends in something");
    func.insert_before(alloca, first);
    let slot = func[alloca].results().next().expect("an alloca has a result");

    let mut offset = 0;
    for &piece in pieces {
        let ty = func[piece].ty;
        let width = bytes(ty);
        let mut address = slot;
        if offset != 0 {
            let imm = func.add_imm(Imm::int(i128::from(offset), Type::int(64)));
            let amount = InstData { extra: Extra::Imm(imm), ..InstData::new(Opcode::IConst) };
            let amount = func.create_inst(amount, &[Type::int(64)], span);
            func.insert_before(amount, call);
            let amount = func[amount].results().next().expect("a constant has a result");
            let add = InstData {
                args: func.push_values(&[slot, amount]),
                ..InstData::new(Opcode::PtrAdd)
            };
            let add = func.create_inst(add, &[Type::PTR], span);
            func.insert_before(add, call);
            address = func[add].results().next().expect("an address has a result");
        }
        let info = MemInfo { size: width, ..info };
        let store = InstData {
            args: func.push_values(&[piece, address]),
            extra: Extra::Mem(func.add_mem(info)),
            ..InstData::new(Opcode::Store)
        };
        let store = func.create_inst(store, &[], span);
        func.insert_before(store, call);
        offset += width.next_multiple_of(8);
    }
    (slot, size)
}

/// An access as the callee described it, less the `restrict` scope, whose numbers are the
/// callee's and could mean a different scope in the caller.
fn unscoped(info: MemInfo) -> MemInfo {
    MemInfo { restrict: Restrict::NONE, ..info }
}

/// A list of branch targets copied across, with the blocks and the arguments mapped.
fn targets(
    func: &mut Func,
    callee: &Func,
    list: BlockCallList,
    blocks: &Map<Block, Block>,
    values: &Map<Value, Value>,
) -> BlockCallList {
    let calls: Vec<BlockCall> = callee[list]
        .iter()
        .map(|call| {
            let args: Vec<Value> = callee[call.args].iter().map(|value| values[value]).collect();
            let args = func.push_values(&args);
            BlockCall { block: blocks[&call.block], args, hint: call.hint }
        })
        .collect();
    func.push_block_calls(&calls)
}

/// Turns every `static` function that nothing in the module refers to any more into a
/// declaration, until there is none left.
///
/// The front end decides which of them to emit from the calls it can see, and a call the passes
/// then found could never run is one it still saw. The kernel writes
/// `if (IS_ENABLED(CONFIG_X86_64)) { ... return ...; } return load_vdso32();`, and gcc emits no
/// `load_vdso32` there, which matters because its body names `vdso32_image`, and nothing in a
/// sixty four bit only kernel defines that. So the question is asked again once the passes are
/// done, and asked again after each answer, since a function taken away may have been the last
/// caller of another. A function that calls itself is still a caller, so one of those stays.
pub fn drop_unreferenced(module: &mut Module) {
    loop {
        let (calls, elsewhere) = references(module);
        let gone: Vec<FuncId> = module
            .funcs()
            .filter(|&id| {
                let func = &module[id];
                !func.is_declaration()
                    && func.linkage == Linkage::Internal
                    && !func.attrs.set.contains(AttrSet::USED)
                    && !calls.contains_key(&func.name)
                    && !elsewhere.contains(&func.name)
            })
            .collect();
        if gone.is_empty() {
            return;
        }
        for id in gone {
            module[id] = declaration(&module[id]);
        }
    }
}

/// Turns every function that still holds a `va_arg_pack` or a `va_arg_pack_len`, and every one
/// marked `inline_only`, into a declaration of the same name.
fn withdraw(module: &mut Module) {
    for id in module.funcs().collect::<Vec<FuncId>>() {
        let func = &module[id];
        if func.is_declaration() {
            continue;
        }
        let holds = func
            .blocks()
            .flat_map(|block| func.insts(block))
            .any(|inst| matches!(func[inst].opcode, Opcode::VaArgPack | Opcode::VaArgPackLen));
        if !holds && !func.attrs.set.contains(AttrSet::INLINE_ONLY) {
            continue;
        }
        module[id] = declaration(func);
    }
}

/// A declaration of that function under the same name, with no body and nothing to emit.
fn declaration(func: &Func) -> Func {
    let mut declared = Func::new(func.name, func.signature().clone());
    declared.spelled = func.spelled;
    declared.visibility = func.visibility;
    declared.attrs = func.attrs;
    declared.target = func.target;
    declared.attrs.set = declared.attrs.set.without(AttrSet::INLINE_ONLY);
    declared.declared = func.declared;
    declared.notices = func.notices.clone();
    declared.linkage = Linkage::External;
    declared
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_diag::Span;

    use super::*;

    const HEAD: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "x86_64-unknown-linux-gnu"
target datalayout = "e-p:64:64-i64:64-f80:128-S128"
"#;

    fn inlined(body: &str) -> String {
        inlined_under(body, None)
    }

    fn inlined_under(body: &str, limit: Option<u32>) -> String {
        inlined_with(body, limit, true).0
    }

    /// The same, with the called once half on or off, and with what the step said about it.
    fn inlined_with(body: &str, limit: Option<u32>, once: bool) -> (String, String) {
        let mut names = Interner::new();
        let text = format!("{HEAD}{body}");
        let mut module = rucc_ir::parse(&text, &mut names).expect("the fixture parses");
        let said = format!(
            "{:?}",
            run(&mut module, &names, limit, once, Isa::baseline(), Growth::DEFAULT, false)
        );
        if let Err(errors) = rucc_ir::verify(&module, &names) {
            panic!("the inliner left invalid IR, {errors:?}\n{}", rucc_ir::print(&module, &names));
        }
        (rucc_ir::print(&module, &names), said)
    }

    /// Two bodies spliced into one caller, each with a buffer of 640 bytes, leave one buffer in the
    /// caller when slots may be shared and two when they may not. A buffer of another size keeps
    /// one of its own, and the caller's own buffer is never taken over.
    #[test]
    fn bodies_spliced_into_one_caller_share_their_slots() {
        let body = r#"
func @use(ptr), linkage(external);

func @part(i32) -> i32, linkage(internal), attrs(always_inline) {
block0(%0: i32):
    %1 = alloca, size 640, align 4
    store %0 -> %1, align 4
    call @use(%1) : (ptr)
    %2 = load.i32 %1, align 4
    return %2
}

func @small(i32) -> i32, linkage(internal), attrs(always_inline) {
block0(%0: i32):
    %1 = alloca, size 64, align 16
    store %0 -> %1, align 4
    call @use(%1) : (ptr)
    %2 = load.i32 %1, align 4
    return %2
}

func @g(i32) -> i32, linkage(external) {
block0(%0: i32):
    %1 = alloca, size 640, align 4
    call @use(%1) : (ptr)
    %2 = call @part(%0) : (i32) -> i32
    %3 = call @part(%2) : (i32) -> i32
    %4 = call @small(%3) : (i32) -> i32
    return %4
}
"#;
        let slots = |share: bool| {
            let mut names = Interner::new();
            let text = format!("{HEAD}{body}");
            let mut module = rucc_ir::parse(&text, &mut names).expect("the fixture parses");
            run(&mut module, &names, None, false, Isa::baseline(), Growth::DEFAULT, share);
            if let Err(errors) = rucc_ir::verify(&module, &names) {
                panic!("the inliner left invalid IR, {errors:?}");
            }
            let g = module.funcs().find(|&id| names.resolve(module[id].name) == "g").expect("g");
            let func = &module[g];
            let mut sizes: Vec<u64> = func
                .blocks()
                .flat_map(|block| func.insts(block))
                .filter(|&inst| func[inst].opcode == Opcode::Alloca)
                .filter_map(|inst| match func[inst].extra {
                    Extra::Mem(mem) => Some(func[mem].size),
                    _ => None,
                })
                .collect();
            sizes.sort_unstable();
            sizes
        };
        assert_eq!(slots(true), [64, 640, 640]);
        assert_eq!(slots(false), [64, 640, 640, 640]);
    }

    /// A callee built for SSE4.2 stays a call from a caller that is not, whether it asked to be
    /// inlined always or only hinted, and goes in where the caller is built for it too.
    #[test]
    fn a_callee_built_for_more_than_its_caller_stays_a_call() {
        let sse42 = "mmx,sse,sse2,sse3,ssse3,sse4.1,sse4.2,popcnt,crc32,fxsr";
        for attrs in ["always_inline", "inline_hint"] {
            let body = format!(
                r#"
func @step(i32) -> i32, linkage(internal), attrs({attrs}), target "{sse42}" {{
block0(%0: i32):
    %1 = add.i32 %0, %0
    return %1
}}

func @plain(i32) -> i32, linkage(external) {{
block0(%0: i32):
    %1 = call @step(%0) : (i32) -> i32
    return %1
}}

func @fast(i32) -> i32, linkage(external), target "{sse42}" {{
block0(%0: i32):
    %1 = call @step(%0) : (i32) -> i32
    return %1
}}
"#
            );
            let (out, said) = inlined_with(&body, Some(100), false);
            let plain = &out
                [out.find("func @plain").expect("plain")..out.find("func @fast").expect("fast")];
            let fast = &out[out.find("func @fast").expect("fast")..];
            assert!(plain.contains("call @step"), "{attrs}: {out}");
            assert!(!fast.contains("call @step"), "{attrs}: {out}");
            assert!(said.contains("not inlined: target specific option mismatch"), "{said}");
        }
    }

    /// The body goes where the call was, its return becomes a jump, and its local goes to the
    /// caller's entry block.
    #[test]
    fn a_call_to_an_always_inline_function_is_replaced_by_its_body() {
        let out = inlined(
            r#"
func @twice(i32) -> i32, linkage(linkonce), attrs(always_inline) {
block0(%0: i32):
    %1 = alloca, size 4, align 4
    %2 = add.i32 %0, %0
    return %2
}

func @g(i32) -> i32, linkage(external) {
block0(%0: i32):
    %1 = call @twice(%0) : (i32) -> i32
    %2 = add.i32 %1, %1
    return %2
}
"#,
        );
        let g = &out[out.find("func @g").expect("g is there")..];
        assert!(!g.contains("call @twice"), "{out}");
        assert!(g.contains("alloca"), "{out}");
    }

    /// A `static` one that every call was inlined into goes, at every level, and one that a call
    /// still reaches, or that `used` keeps, stays.
    #[test]
    fn a_static_always_inline_function_is_not_kept_once_every_call_is_inlined() {
        let body = r#"
func @twice(i32) -> i32, linkage(internal), attrs(always_inline) {
block0(%0: i32):
    %1 = mul.i32 %0, %0
    return %1
}

func @g(i32) -> i32, linkage(external) {
block0(%0: i32):
    %1 = call @twice(%0) : (i32) -> i32
    return %1
}
"#;
        let out = inlined(body);
        assert_eq!(out.matches("mul ").count(), 1, "{out}");
        assert!(!out.contains("linkage(internal)"), "{out}");
        let kept = inlined(&body.replace("attrs(always_inline)", "attrs(always_inline, used)"));
        assert_eq!(kept.matches("mul ").count(), 2, "{kept}");
    }

    /// A `static inline` function every call to which went in goes too, from `-O1` up, since gcc
    /// emits no copy of it and a copy is where a call to a function carrying `error` would survive.
    /// One still called, or kept by `used`, stays, and so does one another unit may call.
    #[test]
    fn a_static_inline_function_is_not_kept_once_every_call_is_inlined() {
        let body = r#"
func @check(i32), linkage(internal), attrs(inline_hint) {
block0(%0: i32):
    call @bad() : ()
    return
}

func @bad(), linkage(external);

func @g() {
block0:
    %0 = iconst.i32 2
    call @check(%0) : (i32)
    return
}
"#;
        let out = inlined_under(body, Some(40));
        assert!(!out.contains("func @check(i32), linkage(internal)"), "{out}");
        let kept = inlined_under(
            &body.replace("attrs(inline_hint)", "attrs(inline_hint, used)"),
            Some(40),
        );
        assert!(kept.contains("func @check(i32), linkage(internal)"), "{kept}");
        let shared = inlined_under(&body.replace("linkage(internal), ", ""), Some(40));
        assert!(shared.contains("block0(%0: i32):\n    call @bad"), "{shared}");
    }

    /// An `i` operand the caller passed as arithmetic over constants is the constant once the call
    /// is inlined, at `-O0` too, the way `_MM_FROUND_TO_NEAREST_INT | _MM_FROUND_NO_EXC` has to be.
    #[test]
    fn an_asm_operand_passed_as_constant_arithmetic_is_the_constant_once_inlined() {
        let out = inlined(
            r#"
func @round(i32), linkage(internal), attrs(always_inline) {
block0(%0: i32):
    inline_asm.volatile "roundps %0, %%xmm0, %%xmm0", "i", "xmm0"(%0)
    return
}

func @g(), linkage(external) {
block0:
    %0 = iconst.i32 8
    %1 = iconst.i32 1
    %2 = or.i32 %0, %1
    call @round(%2) : (i32)
    return
}
"#,
        );
        assert!(out.contains("iconst.i32 9"), "{out}");
        assert!(!out.contains("or."), "{out}");
    }

    /// Gives the instructions of a function those spans, in the order they are laid out.
    fn respan(func: &mut Func, spans: &[Span]) {
        let insts: Vec<Inst> = func.blocks().flat_map(|block| func.insts(block)).collect();
        assert_eq!(insts.len(), spans.len(), "one span for each instruction");
        let mut forward = Map::default();
        for (inst, &span) in insts.into_iter().zip(spans) {
            let data = func[inst];
            let types: Vec<Type> = data.results().map(|value| func[value].ty).collect();
            let new = func.create_inst(data, &types, span);
            let old: Vec<Value> = func[inst].results().collect();
            forward.extend(old.into_iter().zip(func[new].results().collect::<Vec<Value>>()));
            func.insert_before(new, inst);
            func.remove_inst(inst);
        }
        crate::uses::substitute(func, &forward);
    }

    /// The callee's slot and the store of its parameter are at its opening brace, which is outside
    /// the caller. Once inlined, the slot says what the caller's own slots say and the store says
    /// the call, while a statement of the callee keeps its place.
    #[test]
    fn an_inlined_prologue_names_no_line_outside_the_caller() {
        let text = format!(
            r#"{HEAD}
func @twice(i32) -> i32, linkage(internal), attrs(always_inline) {{
block0(%0: i32):
    %1 = alloca, size 4, align 4
    store %0 -> %1, align 4
    %2 = load.i32 %1, align 4
    %3 = add.i32 %2, %2
    return %3
}}

func @g(i32) -> i32, linkage(external) {{
block0(%0: i32):
    %1 = alloca, size 4, align 4
    %2 = call @twice(%0) : (i32) -> i32
    return %2
}}
"#
        );
        let mut names = Interner::new();
        let mut module = rucc_ir::parse(&text, &mut names).expect("the fixture parses");
        let (brace, statement) = (Span::new(10, 50), Span::new(20, 30));
        let (frame, call) = (Span::new(60, 100), Span::new(70, 80));
        let g = names.intern("g");
        for id in module.funcs().collect::<Vec<FuncId>>() {
            let func = &mut module[id];
            if func.name == g {
                func.declared = frame;
                respan(func, &[frame, call, call]);
            } else {
                func.declared = brace;
                respan(func, &[brace, brace, statement, statement, statement]);
            }
        }
        run(&mut module, &names, None, true, Isa::baseline(), Growth::DEFAULT, false);
        let id = module.funcs().find(|&id| module[id].name == g).expect("g is there");
        let func = &module[id];
        let mut seen = Vec::new();
        for block in func.blocks() {
            for inst in func.insts(block) {
                let span = func.span(inst);
                assert_ne!(span, brace, "{:?} kept the callee's brace", func[inst].opcode);
                seen.push((func[inst].opcode, span));
            }
        }
        assert!(seen.iter().all(|&(opcode, span)| opcode != Opcode::Alloca || span == frame));
        assert!(seen.contains(&(Opcode::Store, call)), "{seen:?}");
        assert!(seen.contains(&(Opcode::Load, statement)), "{seen:?}");
    }

    /// The anonymous arguments of the outer call are what the pack stands for, and the out of line
    /// copy that still has one is a declaration afterwards.
    #[test]
    fn a_pack_is_the_anonymous_arguments_of_the_call_inlined() {
        let out = inlined(
            r#"
func @inner(i32, ...) -> i32, linkage(external);

func @wrap(i32, ...) -> i32, linkage(linkonce), attrs(always_inline) {
block0(%0: i32):
    %1 = va_arg_pack.i32
    %2 = call @inner(%0, %1) : (i32, ...) -> i32
    return %2
}

func @g(i64, f64) -> i32, linkage(external) {
block0(%0: i64, %1: f64):
    %2 = iconst.i32 7
    %3 = call @wrap(%2, %0, %1) : (i32, ...) -> i32
    return %3
}
"#,
        );
        assert!(
            out.contains("func @wrap(i32, ...) -> i32, linkage(external), attrs(always_inline);"),
            "{out}"
        );
        assert!(!out.contains("va_arg_pack"), "{out}");
        assert!(out.contains("call @inner(%"), "{out}");
    }

    /// The length is how many anonymous arguments the call had.
    #[test]
    fn a_pack_length_is_the_count_of_the_anonymous_arguments() {
        let out = inlined(
            r#"
func @wrap(i32, ...) -> i32, linkage(linkonce), attrs(always_inline) {
block0(%0: i32):
    %1 = va_arg_pack_len.i32
    return %1
}

func @g(i64, f64) -> i32, linkage(external) {
block0(%0: i64, %1: f64):
    %2 = iconst.i32 7
    %3 = call @wrap(%2, %0, %1) : (i32, ...) -> i32
    return %3
}
"#,
        );
        let g = &out[out.find("func @g").expect("g is there")..];
        assert!(g.contains("iconst.i32 2"), "{out}");
        assert!(!g.contains("call @wrap"), "{out}");
    }

    /// A function declared `inline`, which is a call left alone at `-O0` and inlined above it.
    const HINTED: &str = r#"
func @bump(i32) -> i32, linkage(external), attrs(inline_hint) {
block0(%0: i32):
    %1 = iconst.i32 1
    %2 = add.i32 %0, %1
    return %2
}

func @g(i32) -> i32, linkage(external) {
block0(%0: i32):
    %1 = call @bump(%0) : (i32) -> i32
    return %1
}
"#;

    /// A small function declared `inline` goes in when there is a limit and stays a call when
    /// there is none, which is `-O0`.
    #[test]
    fn a_small_function_declared_inline_is_inlined_above_o0() {
        let out = inlined_under(HINTED, Some(70));
        let g = &out[out.find("func @g").expect("g is there")..];
        assert!(!g.contains("call @bump"), "{out}");
        let out = inlined_under(HINTED, None);
        assert!(out.contains("call @bump"), "{out}");
    }

    /// One that is larger than the limit stays a call.
    #[test]
    fn a_function_declared_inline_over_the_limit_is_left_alone() {
        let out = inlined_under(HINTED, Some(2));
        assert!(out.contains("call @bump"), "{out}");
    }

    /// A `static` function nobody declared `inline`, called from one place.
    const ONCE: &str = r#"
func @scale(i32) -> i32, linkage(internal) {
block0(%0: i32):
    %1 = iconst.i32 3
    %2 = mul.i32 %0, %1
    %3 = iconst.i32 1
    %4 = add.i32 %2, %3
    return %4
}

func @g(i32) -> i32, linkage(external) {
block0(%0: i32):
    %1 = call @scale(%0) : (i32) -> i32
    return %1
}
"#;

    /// Its one call goes in above `-O0` whatever the limit on a function declared `inline` is,
    /// since the out of line copy goes away with it, and stays a call at `-O0`.
    #[test]
    fn a_static_function_called_once_is_inlined_above_o0() {
        let out = inlined_under(ONCE, Some(2));
        let g = &out[out.find("func @g").expect("g is there")..];
        assert!(!g.contains("call @scale"), "{out}");
        assert!(g.contains("mul %0"), "{out}");
        let out = inlined_under(ONCE, None);
        assert!(out.contains("call @scale"), "{out}");
    }

    /// Once its one call is inlined nothing reaches the body, so it goes rather than being emitted
    /// next to the copy.
    #[test]
    fn a_static_function_called_once_is_not_kept_once_inlined() {
        let out = inlined_under(ONCE, Some(2));
        assert_eq!(out.matches("mul ").count(), 1, "{out}");
        assert!(!out.contains("linkage(internal)"), "{out}");
    }

    /// Called from two places it is a function nobody declared `inline`, which stays a call.
    #[test]
    fn a_static_function_called_twice_stays_a_call() {
        let twice = ONCE.replace(
            "    %1 = call @scale(%0) : (i32) -> i32\n    return %1",
            "    %1 = call @scale(%0) : (i32) -> i32\n    %2 = call @scale(%1) : (i32) -> i32\n    \
             return %2",
        );
        assert_ne!(twice, ONCE);
        let out = inlined_under(&twice, Some(70));
        assert_eq!(out.matches("call @scale").count(), 2, "{out}");
    }

    /// One another object can call keeps its copy, so inlining the call here would only grow the
    /// program.
    #[test]
    fn a_function_other_objects_can_call_stays_a_call_when_called_once() {
        let external = ONCE.replace(
            "@scale(i32) -> i32, linkage(internal)",
            "@scale(i32) -> i32, linkage(external)",
        );
        assert_ne!(external, ONCE);
        let out = inlined_under(&external, Some(70));
        assert!(out.contains("call @scale"), "{out}");
    }

    /// Its address is a way to reach it that is not the call, so the copy has to stay and the call
    /// stays with it.
    #[test]
    fn a_static_function_whose_address_is_taken_stays_a_call() {
        let taken = ONCE.replace(
            "    %1 = call @scale(%0) : (i32) -> i32\n    return %1",
            "    %1 = call @scale(%0) : (i32) -> i32\n    %2 = global_addr @scale\n    return %1",
        );
        assert_ne!(taken, ONCE);
        let out = inlined_under(&taken, Some(70));
        assert!(out.contains("call @scale"), "{out}");
    }

    /// `ONCE` with the one call `depth` loops deep in `g`. Each header enters the next loop in or
    /// goes back round the one around it, and the block with the call goes back round the
    /// innermost, so the call is inside every one of them.
    fn nested(depth: usize) -> String {
        let scale = &ONCE[..ONCE.find("func @g").expect("g is there")];
        let (body, out) = (depth + 1, depth + 2);
        let mut g = String::from(
            "func @g(i32, i1) -> i32, linkage(external) {\nblock0(%0: i32, %1: i1):\n    jump block1\n",
        );
        for header in 1..=depth {
            let back = if header == 1 { out } else { header - 1 };
            g += &format!("block{header}:\n    br_if %1, block{}, block{back}\n", header + 1);
        }
        g += &format!(
            "block{body}:\n    %2 = call @scale(%0) : (i32) -> i32\n    jump block{depth}\n"
        );
        g += &format!("block{out}:\n    return %0\n}}\n");
        format!("{scale}{g}")
    }

    /// gcc's `max-inline-functions-called-once-loop-depth` is 6, so a call six loops deep is
    /// inlined and one seven deep is not, with a remark that says why.
    #[test]
    fn a_static_function_called_once_is_inlined_no_more_than_six_loops_deep() {
        let (out, said) = inlined_with(&nested(6), Some(70), true);
        assert!(!out.contains("call @scale"), "{out}");
        assert!(!said.contains("too many loops"), "{said}");
        let (out, said) = inlined_with(&nested(7), Some(70), true);
        assert!(out.contains("call @scale"), "{out}");
        assert!(said.contains("call inside too many loops"), "{said}");
    }

    /// `-fno-inline-functions-called-once` keeps the call that would otherwise go, and leaves the
    /// function declared `inline` to the limit that governs it.
    #[test]
    fn the_called_once_half_can_be_turned_off_alone() {
        let (out, _) = inlined_with(ONCE, Some(70), false);
        assert!(out.contains("call @scale"), "{out}");
        let hinted = ONCE.replace("linkage(internal) {", "linkage(internal), attrs(inline_hint) {");
        assert_ne!(hinted, ONCE);
        let (out, _) = inlined_with(&hinted, Some(70), false);
        assert!(!out.contains("call @scale"), "{out}");
    }

    /// `ONCE` with a local of `callee` bytes in the function called once and one of `caller` bytes
    /// in the function calling it, where either is left out at zero.
    fn framed(callee: u64, caller: u64, attrs: &str) -> String {
        let local = |size: u64, number: u32| {
            if size == 0 {
                String::new()
            } else {
                format!("    %{number} = alloca, size {size}, align 16\n")
            }
        };
        ONCE.replace("linkage(internal) {", &format!("linkage(internal){attrs} {{"))
            .replace("    return %4", &format!("{}    return %4", local(callee, 5)))
            .replace(
                "    %1 = call @scale(%0) : (i32) -> i32\n    return %1",
                &if caller == 0 {
                    "    %1 = call @scale(%0) : (i32) -> i32\n    return %1".to_string()
                } else {
                    format!(
                        "{}    %2 = call @scale(%0) : (i32) -> i32\n    return %2",
                        local(caller, 1)
                    )
                },
            )
    }

    /// A body whose locals would make the caller's frame more than eleven times its own locals, and
    /// more than 256 bytes, stays a call with a remark that says so, which is gcc's
    /// `large-stack-frame-growth` and `large-stack-frame`.
    #[test]
    fn a_body_that_would_grow_the_frame_too_far_stays_a_call() {
        let fixture = framed(4096, 0, "");
        assert_eq!(fixture.matches("alloca").count(), 1, "{fixture}");
        let (out, said) = inlined_with(&fixture, Some(70), true);
        assert!(out.contains("call @scale"), "{out}");
        assert!(said.contains("stack frame growth limit reached"), "{said}");
        let (out, _) = inlined_with(&framed(4096, 256, ""), Some(70), true);
        assert!(out.contains("call @scale"), "{out}");
    }

    /// Small locals always go in, and so do large ones in a caller whose own are large enough.
    #[test]
    fn a_body_that_grows_the_frame_within_the_limit_is_inlined() {
        let (out, _) = inlined_with(&framed(256, 0, ""), Some(70), true);
        assert!(!out.contains("call @scale"), "{out}");
        let fixture = framed(4096, 1024, "");
        assert_eq!(fixture.matches("alloca").count(), 2, "{fixture}");
        let (out, _) = inlined_with(&fixture, Some(70), true);
        assert!(!out.contains("call @scale"), "{out}");
    }

    /// Under `-fconserve-stack` the frame may grow by 40 percent or to 100 bytes, gcc's numbers
    /// there, so both of the bodies the default lets in above stay calls, and a small one goes in.
    #[test]
    fn conserving_the_stack_keeps_bodies_out_that_the_default_lets_in() {
        let conserved = |fixture: &str| {
            let mut names = Interner::new();
            let text = format!("{HEAD}{fixture}");
            let mut module = rucc_ir::parse(&text, &mut names).expect("the fixture parses");
            run(&mut module, &names, Some(70), true, Isa::baseline(), Growth::CONSERVE, false);
            rucc_ir::print(&module, &names)
        };
        assert!(conserved(&framed(256, 0, "")).contains("call @scale"));
        assert!(conserved(&framed(4096, 1024, "")).contains("call @scale"));
        assert!(!conserved(&framed(96, 0, "")).contains("call @scale"));
        assert!(!conserved(&framed(400, 1024, "")).contains("call @scale"));
    }

    /// `always_inline` is a promise and the frame does not change that.
    #[test]
    fn an_always_inline_body_goes_in_whatever_it_does_to_the_frame() {
        let (out, _) = inlined_with(&framed(4096, 0, ", attrs(always_inline)"), Some(70), true);
        let g = &out[out.find("func @g").expect("g is there")..];
        assert!(!g.contains("call @scale"), "{out}");
    }

    /// `noinline` is kept whoever calls it how often.
    #[test]
    fn a_static_function_called_once_and_marked_noinline_stays_a_call() {
        let kept = ONCE.replace("linkage(internal) {", "linkage(internal), attrs(noinline) {");
        assert_ne!(kept, ONCE);
        let out = inlined_under(&kept, Some(70));
        assert!(out.contains("call @scale"), "{out}");
    }

    /// Each copy of a body that takes the address of its own label gets a label of its own, which
    /// is `990208-1.c`.
    #[test]
    fn each_copy_of_a_label_address_is_a_label_of_its_own() {
        let out = inlined_under(
            r#"
func @here() -> ptr, linkage(internal), attrs(inline_hint) {
block0:
    jump block1
block1:
    %0 = block_addr block1
    return %0
}

func @g() -> i1, linkage(external) {
block0:
    %0 = call @here() : () -> ptr
    %1 = call @here() : () -> ptr
    %2 = icmp eq %0, %1
    return %2
}
"#,
            Some(70),
        );
        let g = &out[out.find("func @g").expect("g is there")..];
        assert!(!g.contains("call @here"), "{out}");
        assert_eq!(g.matches("block_addr").count(), 2, "{out}");
    }

    /// A body that jumps through a label address is refused, since a table of them may be what
    /// it jumps through and the table names the original body.
    #[test]
    fn a_computed_goto_is_not_inlined() {
        let out = inlined_under(
            r#"
func @jump(ptr) -> i32, linkage(internal), attrs(inline_hint) {
block0(%0: ptr):
    indirect_br %0, block1
block1:
    %1 = iconst.i32 1
    return %1
}

func @g(ptr) -> i32, linkage(external) {
block0(%0: ptr):
    %1 = call @jump(%0) : (ptr) -> i32
    return %1
}
"#,
            Some(70),
        );
        assert!(out.contains("call @jump"), "{out}");
    }

    /// A `static` function called once that saves a place to come back to through `callee`,
    /// declared as `declared`, the way Postgres's `PG_TRY` does with `sigsetjmp`.
    fn guarded(callee: &str, declared: &str) -> String {
        format!(
            r#"
func @{callee}(ptr, i32) -> i32, linkage(external){declared};

func @fetch(ptr) -> i32, linkage(internal) {{
block0(%0: ptr):
    %1 = iconst.i32 0
    %2 = call @{callee}(%0, %1) : (ptr, i32) -> i32
    return %2
}}

func @g(ptr) -> i32, linkage(external) {{
block0(%0: ptr):
    %1 = call @fetch(%0) : (ptr) -> i32
    return %1
}}
"#
        )
    }

    /// glibc's `sigsetjmp` is a macro for `__sigsetjmp`, and a body calling it stays a call, so
    /// its `jmp_buf` stays in its own frame rather than growing the caller's on every path.
    #[test]
    fn a_body_that_calls_sigsetjmp_stays_a_call() {
        let (out, said) = inlined_with(&guarded("__sigsetjmp", ""), Some(70), true);
        let g = &out[out.find("func @g").expect("g is there")..];
        assert!(g.contains("call @fetch"), "{out}");
        assert!(said.contains("callee calls setjmp"), "{said}");
    }

    /// The same for a function of any name declared `returns_twice`.
    #[test]
    fn a_body_that_calls_a_function_declared_returns_twice_stays_a_call() {
        let out = inlined_under(&guarded("save_here", ", attrs(returns_twice)"), Some(70));
        let g = &out[out.find("func @g").expect("g is there")..];
        assert!(g.contains("call @fetch"), "{out}");
    }

    /// And without the attribute the call is an ordinary one, so the body goes in.
    #[test]
    fn a_body_that_calls_an_ordinary_function_is_inlined() {
        let out = inlined_under(&guarded("save_here", ""), Some(70));
        let g = &out[out.find("func @g").expect("g is there")..];
        assert!(!g.contains("call @fetch"), "{out}");
        assert!(g.contains("call @save_here"), "{out}");
    }

    /// A function that reaches itself is left as a call rather than unrolled for ever.
    #[test]
    fn a_recursive_always_inline_function_is_left_alone() {
        let out = inlined(
            r#"
func @r(i32) -> i32, linkage(linkonce), attrs(always_inline) {
block0(%0: i32):
    %1 = call @r(%0) : (i32) -> i32
    return %1
}
"#,
        );
        assert!(out.contains("call @r("), "{out}");
    }

    /// Two general purpose registers of a structure that the second call has one left for, which
    /// goes to memory instead.
    #[test]
    fn a_structure_that_would_straddle_the_registers_goes_to_memory() {
        let int = (Type::int(64), Abi::Plain);
        let outer = [int];
        let before = [int, int, int, int, int];
        let forwarded = [int, int];
        let sysv = |before: &[(Type, Abi)], groups| {
            forwardable(Convention::SysV, &outer, before, &forwarded, groups)
        };
        assert_eq!(sysv(&before, Some(&[2])), Some(vec![0]));
        assert_eq!(sysv(&before, Some(&[1, 1])), Some(Vec::new()));
        assert_eq!(sysv(&before, None), None);
        assert_eq!(sysv(&outer, None), Some(Vec::new()));
    }

    /// A function with a `cleanup` handler of its own, called from inside the scope of one of the
    /// caller's, both under `-fexceptions`. The lowering gives each call in a handler's scope an
    /// `unwound` and a branch on it to a pad that runs the handlers and resumes the unwind.
    const PADDED: &str = r#"
func @hinted(i32), linkage(internal), attrs(inline_hint) {
block0(%0: i32):
    %1 = alloca, size 4, align 4
    %2 = iconst.i32 5
    store %2 -> %1, align 4
    call @leave(%0) : (i32)
    %3 = unwound.i1
    br_if %3, block1, block2

block1:
    %4 = landing.ptr
    call @done(%1) : (ptr)
    call @_Unwind_Resume(%4) : (ptr)
    unreachable

block2:
    call @done(%1) : (ptr)
    return
}

func @outer(i32), linkage(external) {
block0(%0: i32):
    %1 = alloca, size 4, align 4
    %2 = iconst.i32 1
    store %2 -> %1, align 4
    call @hinted(%0) : (i32)
    %3 = unwound.i1
    br_if %3, block1, block2

block1:
    %4 = landing.ptr
    call @done(%1) : (ptr)
    call @_Unwind_Resume(%4) : (ptr)
    unreachable

block2:
    call @done(%1) : (ptr)
    return
}
"#;

    /// Inlined, the body's calls are calls the caller's pad has to cover, since an unwind out of
    /// any of them passes through the call that was there. The one with a pad of its own keeps it,
    /// and that pad's `_Unwind_Resume` is covered like the rest, which is how the unwind gets from
    /// the callee's handler to the caller's. The `unwound` of the call that went has nothing left
    /// to ask about and goes with it.
    #[test]
    fn a_call_with_a_landing_pad_is_inlined_and_the_pad_covers_the_body() {
        let out = inlined_under(PADDED, Some(70));
        let outer = &out[out.find("func @outer").expect("outer is there")..];
        assert!(!outer.contains("call @hinted"), "{out}");
        // The callee's own edge, and one for each of the three calls in the body that had none:
        // the handler on the way out, the handler in its pad and the resume after it.
        assert_eq!(outer.matches("= unwound").count(), 4, "{outer}");
        assert_eq!(outer.matches("= landing").count(), 2, "{outer}");
        assert_eq!(outer.matches("call @_Unwind_Resume").count(), 2, "{outer}");
        // The caller's pad is where three of those edges go, and the fourth is the callee's.
        assert_eq!(outer.matches(", block1, ").count(), 3, "{outer}");
        // At -O0 nothing is inlined, pad or not.
        assert!(inlined(PADDED).contains("call @hinted"));
    }

    /// A call to a body that never returns is the end of the block it was in, so what came after
    /// it in the caller is reached by nothing once the body is in, and the step deletes it rather
    /// than leave it for the verifier to find. This is `spin` from #2058, a `static` function with
    /// an empty `for (;;)` called under a condition the program never meets.
    #[test]
    fn what_follows_a_body_that_never_returns_is_deleted() {
        let out = inlined_under(
            r#"
func @spin(), linkage(internal) {
block0:
    jump block1
block1:
    jump block1
}

func @main(i1) -> i32, linkage(external) {
block0(%0: i1):
    %1 = iconst.i32 1
    br_if %0, block1, block2(%1)
block1:
    call @spin() : ()
    %2 = iconst.i32 100
    jump block2(%2)
block2(%3: i32):
    return %3
}
"#,
            Some(15),
        );
        let main = &out[out.find("func @main").expect("main")..];
        assert!(!main.contains("call @spin"), "{out}");
        assert!(!main.contains("100"), "{out}");
    }
}
