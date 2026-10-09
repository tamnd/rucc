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
//! `-fno-inline-functions-called-once` turns it off alone. It is a second round over the module,
//! after every other kind of call is in and with the calls counted again, because that is when gcc
//! takes it, and a function that a smaller one calls once is called once per copy of the smaller
//! one by then.
//!
//! A round after that takes a `static` function called from more than one place when copying it
//! into every caller leaves the program no larger, which is gcc's
//! `want_inline_function_to_all_callers_p` (tamnd/rucc#3150). It comes after so that a body is
//! weighed with what it called once already in it. Each copy costs the body as the constants its
//! call passes leave it, weighed as gcc weighs it and read off the summary, less the call it
//! replaces, and the copies together are weighed against the body, which goes once the last of
//! them is in. gcc copies none of the calls unless it can copy every one, so a call too many loops
//! deep, in a function that asked not to be optimized or built for less than the callee, or a
//! callee that calls itself, keeps them all calls.
//!
//! A body that takes the address of one of its own labels is copied with the label, so each copy
//! has an address of its own, which is what gcc does and what `990208-1.c` checks. A body that
//! jumps to such an address, or whose labels a static table holds, is refused, since the copy
//! would still be reaching into the original.

use std::cell::RefCell;

use rucc_base::hash::{Map, Set};
use rucc_base::{Interner, Symbol};
use rucc_cost::heuristics::{
    INLINE_CALL_TIME, INLINE_CALLED_ONCE_INSNS, INLINE_CALLED_ONCE_LOOP_DEPTH, INLINE_EARLY_INSNS,
    INLINE_EARLY_INSNS_O3, INLINE_FRAME_GROWTH, INLINE_FRAME_GROWTH_CONSERVE, INLINE_INSNS_AUTO,
    INLINE_INSNS_AUTO_O3, INLINE_INSNS_SINGLE, INLINE_INSNS_SINGLE_O3, INLINE_LARGE_FRAME,
    INLINE_LARGE_FRAME_CONSERVE,
};
use rucc_diag::Span;
use rucc_ir::{
    Abi, AsmInfo, AttrSet, Block, BlockCall, BlockCallList, CallInfo, Copies, DataLayout, Datum,
    Def, Drains, Extra, Flags, Float, Func, FuncId, GlobalId, Imm, Inst, InstData, Linkage,
    MemInfo, MemOrder, Module, Opcode, Pic, Restrict, Signature, SwitchInfo, Type, VaInfo, Value,
    ValueList,
};
use rucc_target::{Isa, TargetInfo};

use crate::Stats;
use crate::callgraph::trusted;
use crate::cfg::Cfg;
use crate::dom::Dominators;
use crate::loops::Loops;

mod heap;
mod summary;

pub use heap::Second;

/// What the step calls itself in a remark, and the name `-fno-inline` turns the declared half off
/// by.
pub const NAME: &str = "inline";

/// What `-finline-functions-called-once` and its `-fno-` form toggle, which is the called once half
/// of this step alone. Not the name of a pass.
pub const ONCE: &str = "inline-functions-called-once";

/// What `-finline-small-functions` and its `-fno-` form toggle, which is the half that takes a
/// small function nobody declared `inline`. Not the name of a pass.
pub const SMALL: &str = "inline-small-functions";

const INLINED: &str = "always_inline call inlined";

const HINT_INLINED: &str = "inline call inlined";

const ONCE_INLINED: &str = "call to a static function called once inlined";

const ALL_INLINED: &str = "call to a static function inlined into all its callers";

const ASKS_INLINED: &str = "call passing a constant __builtin_constant_p asks about inlined";

const SMALL_INLINED: &str = "call to a function no larger than the call inlined";

const AUTO_INLINED: &str = "call to a small function inlined";
/// A call weighed by less than the callee's whole body, since the constants it passes remove some of
/// it. See [`summary::Summary`].
const CUT: &str = "call weighed without the code its constant arguments remove";

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
    /// A `static` function that asks `__builtin_constant_p` about one of its parameters, inlined
    /// at a call that passes a constant there when the body is as small as a hinted one.
    ///
    /// gcc's inliner counts what an answer would take out of the body, so a call like that is one
    /// it takes. The kernel relies on it: i915's `hwm_field_read_and_scale` hands its mask to
    /// `REG_FIELD_GET`, whose `BUILD_BUG_ON` only goes away once the mask is the constant each of
    /// its two callers passes.
    Asks,
    /// A function whose body is no larger than a call to it, inlined wherever it is called
    /// whatever its linkage, as long as the body is the one that runs.
    ///
    /// gcc's early inliner takes a call when the copy does not grow the caller, and it does that
    /// for a function other files call as well, keeping the out of line copy for them. The kernel
    /// relies on it: `mm/shmem.c` in 5.15 and 6.1 sets a field to a `BUILD_BUG` behind a call to
    /// `shmem_is_huge`, which with transparent huge pages off is a non-`static` function returning
    /// false, and the build only links when that call is folded away.
    Small,
    /// A function nobody declared `inline` whose copy grows the caller by less than
    /// `max-inline-insns-auto`, which is gcc's `-finline-small-functions`, on from `-O2`.
    ///
    /// The kernel is where it shows. A `static` helper like `reserve_space` in fs/nfs/nfs4xdr.c,
    /// three lines around a `BUG_ON`, is copied into each of its callers by gcc, and each copy is
    /// an entry in `__bug_table` that the object has under gcc and not without it.
    Auto,
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
    /// The caller or the callee is written `cold` and the program would come out larger with the
    /// body copied in. Only a call that is not `always_inline` or to a function called once is
    /// refused for this.
    Unlikely,
    /// The second pass would grow the whole unit past what `inline-unit-growth` lets it.
    UnitGrowth,
    /// The second pass would grow a large caller past what `large-function-growth` lets it.
    FunctionGrowth,
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
            Self::Unlikely => {
                "always_inline call not inlined: call is unlikely and code size would grow"
            }
            Self::UnitGrowth => "always_inline call not inlined: unit growth limit reached",
            Self::FunctionGrowth => "always_inline call not inlined: function growth limit reached",
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
            Self::Unlikely => "inline call not inlined: call is unlikely and code size would grow",
            Self::UnitGrowth => "inline call not inlined: unit growth limit reached",
            Self::FunctionGrowth => "inline call not inlined: function growth limit reached",
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
            Self::Unlikely => {
                "call to a function called once not inlined: call is unlikely and code size would grow"
            }
            Self::UnitGrowth => {
                "call to a function called once not inlined: unit growth limit reached"
            }
            Self::FunctionGrowth => {
                "call to a function called once not inlined: function growth limit reached"
            }
        }
    }
}

/// gcc's limits on the inliner at one level, which are the same at every level but `-O3`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// The largest body a callee declared `inline` may have. gcc's `max-inline-insns-single`.
    pub single: u32,
    /// How much a call to a function nobody declared `inline` may grow its caller. gcc's
    /// `max-inline-insns-auto`.
    pub auto: u32,
    /// How much a call may grow a caller it is not hot in. gcc's `early-inlining-insns`.
    pub early: u32,
}

impl Limits {
    /// gcc's numbers at `-O1`, `-O2` and `-Os`, with what `--param` set.
    #[must_use]
    pub fn o2() -> Self {
        Self {
            single: rucc_cost::param!(INLINE_INSNS_SINGLE),
            auto: rucc_cost::param!(INLINE_INSNS_AUTO),
            early: rucc_cost::param!(INLINE_EARLY_INSNS),
        }
    }

    /// gcc's numbers at `-O3`, where each of the three is larger.
    #[must_use]
    pub fn o3() -> Self {
        Self {
            single: rucc_cost::param!(INLINE_INSNS_SINGLE_O3),
            auto: rucc_cost::param!(INLINE_INSNS_AUTO_O3),
            early: rucc_cost::param!(INLINE_EARLY_INSNS_O3),
        }
    }
}

/// Inlines every call to an `always_inline` function that can be, and with `limit` every call to
/// a function declared `inline` whose body is no larger than it allows, every call to a function whose
/// body is no larger than the call and, when `once` says so, the one call to a `static` function
/// called once, and says what it did where.
///
/// Then turns every function still holding a `va_arg_pack` into a declaration. See the module
/// documentation for why that is the right thing to do with one.
///
/// `isa` is what the module is built for, which is what a function without a `target` attribute
/// is built for. A callee built for more than its caller is never copied into it. `names` is
/// what the names of the functions a body calls are read from, to find a call to `setjmp`.
/// `growth` is how far a caller's frame may grow, which `-fconserve-stack` makes tighter. `pic` is
/// what says whether the body of a function other files see is the one a call reaches. With
/// `second_pass` the calls the first pass finds too large are weighed again by the second, see
/// [`Second`].
#[allow(clippy::too_many_arguments)]
pub fn run(
    module: &mut Module,
    names: &Interner,
    limit: Option<Limits>,
    once: bool,
    isa: Isa,
    growth: Growth,
    share: bool,
    pic: Pic,
    auto: bool,
    second_pass: Option<Second>,
) -> Vec<(FuncId, Stats)> {
    // A call through a member of a `static const` table of operations, or through a pointer that
    // can only be one function, is a call to that function by the time gcc decides what to inline,
    // since its early passes have folded the load and the call. So it is here too, before any of
    // the calls are counted. See [`crate::image`].
    if limit.is_some() {
        let images = crate::image::Images::of(module, pic);
        if !images.is_empty() {
            for id in module.funcs().collect::<Vec<FuncId>>() {
                if !module[id].is_declaration() {
                    let mut stats = Stats::new();
                    let mut fuel = crate::Fuel::unlimited();
                    crate::image::settle(&mut module[id], &images, &mut fuel, &mut stats);
                }
            }
        }
    }
    // Which kind of call each function is inlined by, given the names called once.
    let classify = |module: &Module, once: &Set<Symbol>| -> Map<Symbol, (FuncId, Kind)> {
        module
            .funcs()
            .filter(|&id| !module[id].is_declaration())
            .filter_map(|id| {
                let func = &module[id];
                let set = func.attrs.set;
                // `noipa` is never inlined, whatever else was written.
                let kind = if set.contains(AttrSet::NOIPA) {
                    return None;
                } else if set.contains(AttrSet::ALWAYS_INLINE) {
                    Kind::Always
                } else if limit.is_none()
                    || set.without(
                        AttrSet::NOINLINE | AttrSet::OPTNONE | AttrSet::NAKED | AttrSet::INTERRUPT,
                    ) != set
                {
                    return None;
                } else if func.linkage == Linkage::Internal
                    && !set.contains(AttrSet::USED)
                    && once.contains(&func.name)
                {
                    Kind::Once
                } else if set.contains(AttrSet::INLINE_HINT) {
                    Kind::Hinted
                } else if func.linkage == Linkage::Internal
                    && !set.contains(AttrSet::USED)
                    && !asked(func).is_empty()
                {
                    Kind::Asks
                } else if small(func) && trusted(func, pic) {
                    Kind::Small
                } else if auto && trusted(func, pic) {
                    Kind::Auto
                } else {
                    return None;
                };
                Some((func.name, (id, kind)))
            })
            .collect()
    };
    // Two rounds, in the order gcc takes them. The first is everything but the called once rule,
    // and the second is that rule over the calls the first left, counted again. gcc inlines a
    // function called once only after the small functions are in, so a `static inline` helper
    // called from four places is measured before the one large function it calls is folded into
    // it, and once it is in all four that function is called four times and stays out of line.
    // kernel/locking/semaphore.c is that shape: `__down_common` holds two tracepoints and a call
    // to `___down_common`, and gcc has the tracepoints in each of `__down` and its siblings.
    let wanted = classify(module, &Set::default());
    let mut done = Vec::new();
    let convention = Convention::of(module);
    let most = limit.map_or(0, |limit| usize::try_from(limit.single).unwrap_or(usize::MAX));
    let limits = limit.unwrap_or_else(Limits::o2);
    // The frames the functions have before anything is inlined into them, which is what the second
    // pass measures a caller's frame against, as the first measures each caller's own.
    let own: Map<FuncId, u64> = if second_pass.is_some() {
        module
            .funcs()
            .filter(|&id| !module[id].is_declaration())
            .map(|id| (id, frame(&module[id], module.datalayout)))
            .collect()
    } else {
        Map::default()
    };
    // What the bodies measured, kept from one round to the next. See [`How::sizes`].
    let sizes = RefCell::default();
    let round = |module: &mut Module,
                 wanted: &Map<Symbol, (FuncId, Kind)>,
                 done: &mut Vec<_>,
                 later: Option<&RefCell<heap::Later>>| {
        if wanted.is_empty() {
            return;
        }
        let (calls, elsewhere) = references(module);
        let cold = unlikely(module, names);
        let how = How {
            wanted,
            convention,
            limit: most,
            auto: limits.auto,
            early: limits.early,
            isa,
            names,
            growth,
            share,
            calls: &calls,
            cold: &cold,
            elsewhere: &elsewhere,
            later,
            sizes: &sizes,
            summaries: RefCell::default(),
        };
        let mut state = Map::default();
        for id in module.funcs().collect::<Vec<FuncId>>() {
            settle(module, id, &how, &mut state, done);
        }
    };
    // gcc's early inliner goes first, taking each call to a function no larger than the call while
    // that function is still the few lines it was written as, before anything is inlined into it.
    // What the body calls is then a call in the caller, and measured there. In mm/mmap_lock.c,
    // `__mmap_lock_trace_start_locking` calls `__mmap_lock_do_trace_start_locking`, which is one
    // line calling a tracepoint, and gcc has the tracepoint in each caller, each one an entry in
    // `__jump_table`. Settled first, that one line is the whole tracepoint and far larger than a
    // call.
    //
    // The early inliner takes the `always_inline` calls in a function before it weighs the small
    // ones, and it works from the callees up, so a function whose one line calls an
    // `always_inline` body is that body by the time a caller asks how large it is. zstd's
    // `HUF_DGEN` wrappers are that shape, one line each around a `FORCE_INLINE_TEMPLATE` decoder,
    // and gcc keeps them out of line where they were copied into every caller, about 18KB a copy
    // on i386.
    let mut small = wanted.clone();
    small.retain(|_, &mut (_, kind)| matches!(kind, Kind::Small | Kind::Always));
    round(module, &small, &mut done, None);
    let mut wanted = small;
    wanted.extend(classify(module, &Set::default()));
    // gcc's second inliner comes after its early one and before the called once rule, and weighs
    // what the early one found too large again, so this round writes those down rather than
    // refusing them.
    let later = RefCell::new(heap::Later::default());
    let second_pass = second_pass.filter(|_| limit.is_some());
    round(module, &wanted, &mut done, second_pass.and(Some(&later)));
    if let Some(second) = second_pass {
        let later = later.into_inner();
        let (calls, elsewhere) = references(module);
        let cold = unlikely(module, names);
        let how = How {
            wanted: &wanted,
            convention,
            limit: most,
            auto: limits.auto,
            early: limits.early,
            isa,
            names,
            growth,
            share,
            calls: &calls,
            cold: &cold,
            elsewhere: &elsewhere,
            later: None,
            sizes: &sizes,
            summaries: RefCell::default(),
        };
        done.extend(heap::run(module, &how, &later, &own, second, pic));
        // The heap takes the blocks its copies stranded out of each caller once it is finished,
        // which changes a body without making it any larger, so nothing measured before that is
        // kept past it.
        sizes.borrow_mut().clear();
    }
    if limit.is_some() && once {
        // A function the heap copied into its last caller still holds the calls in its body until
        // it goes, and a function one of those reaches would not look called once while it does.
        // gcc's inliner drops such a function as soon as its last call is in.
        while bury(module, &wanted) {}
        let mut second = classify(module, &called_once(module));
        second.retain(|_, &mut (_, kind)| kind == Kind::Once);
        round(module, &second, &mut done, None);
        wanted.extend(second);
        // gcc weighs a function for all its callers as it is by then, with what it called once
        // already in it, so this round waits for that one and for its bodies to go.
        while bury(module, &wanted) {}
        let mut all = classify(module, &to_all_callers(module, names, isa));
        all.retain(|_, &mut (_, kind)| kind == Kind::Once);
        round(module, &all, &mut done, None);
        wanted.extend(all);
    }
    if !wanted.is_empty() {
        bury(module, &wanted);
        for &(id, _) in &done {
            settle_operands(&mut module[id]);
        }
    }
    withdraw(module);
    done
}

/// Makes a declaration of each function the passes inlined that nothing refers to any more and
/// that may go, and says whether there was one.
fn bury(module: &mut Module, wanted: &Map<Symbol, (FuncId, Kind)>) -> bool {
    let (calls, elsewhere) = references(module);
    let mut buried = false;
    for &(id, kind) in wanted.values() {
        let func = &module[id];
        let name = func.name;
        let gone = match kind {
            Kind::Once => true,
            Kind::Always | Kind::Hinted | Kind::Asks | Kind::Small | Kind::Auto => {
                func.linkage == Linkage::Internal && !func.attrs.set.contains(AttrSet::USED)
            }
        };
        if gone
            && !func.is_declaration()
            && !calls.contains_key(&name)
            && !elsewhere.contains(&name)
        {
            module[id] = declaration(&module[id]);
            buried = true;
        }
    }
    buried
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
        data.flags = Flags::NONE;
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

/// The `static` functions reached by more than one direct call and in no other way whose copies,
/// one into each caller, come to no more than the body, which goes once they are all in. That is
/// gcc's `want_inline_function_to_all_callers_p`.
///
/// A copy costs the body as the constants its call passes leave it, read off the summary, less the
/// call it replaces, and the sum is weighed against the body with nothing known, which is what
/// gcc's `growth_positive_p` asks. Both sides are weighed as gcc's `estimate_num_insns` weighs
/// them, since a call is one and one more for each argument and for the result to gcc. Counted as
/// one apiece, a body that only passes six arguments on looked smaller than the call of one
/// argument it replaced, and SQLite grew by two percent at `-O1` with copies gcc does not make.
/// gcc takes none of the calls unless it can take every one, which is `check_callers`, so a
/// function is left out when one of its calls is in itself, in a function that asked not to be
/// optimized, in one built for less than the callee, or more than
/// `max-inline-functions-called-once-loop-depth` loops deep. One called from another this round
/// may take is left out as well, since copying that one copies the call and the count is off.
/// Whether it is a function that may be inlined at all is for the caller to ask, as with
/// [`called_once`].
fn to_all_callers(module: &Module, names: &Interner, isa: Isa) -> Set<Symbol> {
    let (counts, elsewhere) = references(module);
    let mut wanted: Map<Symbol, FuncId> = Map::default();
    for id in module.funcs() {
        let func = &module[id];
        if !func.is_declaration()
            && func.linkage == Linkage::Internal
            && !func.attrs.set.contains(AttrSet::USED)
            && !elsewhere.contains(&func.name)
            && counts.get(&func.name).is_some_and(|&count| count > 1)
        {
            wanted.insert(func.name, id);
        }
    }
    if wanted.is_empty() {
        return Set::default();
    }
    let mut sites: Map<Symbol, Vec<(FuncId, Block, Inst)>> = Map::default();
    for id in module.funcs() {
        let func = &module[id];
        for block in func.blocks() {
            for inst in func.insts(block) {
                let Extra::Call(info) = func[inst].extra else { continue };
                if func[inst].opcode != Opcode::Call {
                    continue;
                }
                if let Some(name) = func[info].callee.filter(|name| wanted.contains_key(name)) {
                    sites.entry(name).or_default().push((id, block, inst));
                }
            }
        }
    }
    let mut chosen = Set::default();
    // The loops of each caller, built the first time one of its calls is asked about. Nothing
    // here edits the module, and a caller that calls a dozen of these functions built its forest
    // once for each of the calls, which on duktape.c at `-O1` was two percent of the compile.
    // tamnd/rucc#3052.
    let mut forests: Map<FuncId, Loops> = Map::default();
    for (name, calls) in sites {
        let id = wanted[&name];
        let callee = &module[id];
        let refused = |caller: FuncId| {
            let func = &module[caller];
            caller == id
                || wanted.contains_key(&func.name)
                || func.attrs.set.contains(AttrSet::OPTNONE)
                || callee.target.is_some_and(|wanted| !func.target.unwrap_or(isa).covers(wanted))
        };
        if calls.iter().any(|&(caller, ..)| refused(caller)) {
            continue;
        }
        let summary = summary::Summary::of(callee, names, None);
        let mut growth = 0_i64;
        for &(caller, _, call) in &calls {
            let func = &module[caller];
            let (copy, _) = summed_size(&summary, callee, passed(func, call, callee), Some(names));
            // gcc counts the result of a call it has a place for, which is one that has a result.
            let cost = weight(func, call, names, func[call].results().next().is_some());
            growth += i64::try_from(copy).unwrap_or(i64::MAX) - i64::try_from(cost).unwrap_or(0);
        }
        // The body is weighed the way its copies are, as a copy no constant cuts down.
        let (whole, _) = summed_size(&summary, callee, Map::default(), Some(names));
        if growth > i64::try_from(whole).unwrap_or(i64::MAX) {
            continue;
        }
        // Counted as [`settle`] counts, so a block in one loop is one deep.
        let deep = calls.iter().any(|&(caller, block, _)| {
            let loops = forests.entry(caller).or_insert_with(|| {
                let cfg = Cfg::new(&module[caller]);
                Loops::new(&cfg, &Dominators::new(&cfg))
            });
            let depth = loops.innermost(block).map_or(0, |inner| loops.depth(inner) + 1);
            depth > rucc_cost::param!(INLINE_CALLED_ONCE_LOOP_DEPTH)
        });
        if !deep {
            chosen.insert(name);
        }
    }
    chosen
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

/// How many of the direct calls to each name are ones gcc does not think of as hot, which are the
/// calls made from a function written `cold` and the calls a function that [`runs_once`] makes
/// outside its loops.
fn unlikely(module: &Module, names: &Interner) -> Map<Symbol, usize> {
    let mut cold: Map<Symbol, usize> = Map::default();
    for id in module.funcs() {
        let func = &module[id];
        if func.is_declaration() {
            continue;
        }
        let made: Vec<Inst> = if func.attrs.set.contains(AttrSet::COLD) {
            func.blocks().flat_map(|block| func.insts(block)).collect()
        } else if runs_once(func, names) {
            flat(func).into_iter().collect()
        } else {
            continue;
        };
        for inst in made {
            let Extra::Call(info) = func[inst].extra else { continue };
            if func[inst].opcode != Opcode::Call {
                continue;
            }
            if let Some(callee) = func[info].callee {
                *cold.entry(callee).or_default() += 1;
            }
        }
    }
    cold
}

/// Whether gcc says a function runs once each time the program does before it has seen who calls
/// it, which it says of `main` and of a function that never comes back, unless either is written
/// `hot`. That is `compute_function_frequency` with no profile, and a call such a function makes
/// outside its loops is one `cgraph_edge::maybe_hot_p` says is not hot.
fn runs_once(func: &Func, names: &Interner) -> bool {
    let main = func.linkage != Linkage::Internal && names.resolve(func.name) == "main";
    !func.attrs.set.contains(AttrSet::HOT) && (main || func.attrs.set.contains(AttrSet::NORETURN))
}

/// The direct calls a function makes outside its loops.
fn flat(func: &Func) -> Set<Inst> {
    let cfg = Cfg::new(func);
    let loops = Loops::new(&cfg, &Dominators::new(&cfg));
    func.blocks()
        .filter(|&block| loops.innermost(block).is_none())
        .flat_map(|block| func.insts(block))
        .filter(|&inst| func[inst].opcode == Opcode::Call)
        .collect()
}

/// What stays the same for every function [`settle`] visits.
struct How<'a> {
    /// The functions whose calls are inlined, by name, and why.
    wanted: &'a Map<Symbol, (FuncId, Kind)>,
    /// The calling convention the pack is forwarded under.
    convention: Convention,
    /// How many instructions a callee declared `inline` may have.
    limit: usize,
    /// How much a call to a function nobody declared `inline` may grow its caller.
    auto: u32,
    /// How much a call may grow a caller it is not hot in whatever else is known.
    early: u32,
    /// What a function without a `target` attribute of its own is built for.
    isa: Isa,
    /// What the names in the module are read from.
    names: &'a Interner,
    /// How far a caller's frame may grow.
    growth: Growth,
    /// Whether bodies spliced into the same caller may share their slots. See [`Pool`].
    share: bool,
    /// How many direct calls the module makes to each name before anything is inlined.
    calls: &'a Map<Symbol, usize>,
    /// How many of those calls are not hot, see [`unlikely`].
    cold: &'a Map<Symbol, usize>,
    /// The names the module reaches other than by a direct call.
    elsewhere: &'a Set<Symbol>,
    /// Where the calls left for the second pass are written down, when there is one.
    later: Option<&'a RefCell<heap::Later>>,
    /// What [`specialized_size`] said, by callee, how large its tables were, the constants it was
    /// given and whether it weighed, so a body called from four hundred places with the same
    /// constants is copied and folded once.
    ///
    /// Kept across the rounds rather than for one, since a callee the first round settled and the
    /// second finds nothing more to copy into is the same body both times, and on monocypher.c at
    /// `-O2` measuring the same few large ones again in each round was a third of the build.
    /// tamnd/rucc#3052. A callee is settled before it is measured, and what changes one after that
    /// is a copy into it, which only ever adds to its tables, so an answer under the sizes it had
    /// then is an answer about the body it has now. The one edit that takes away without adding
    /// is the sweep after the heap, and the cache is emptied there.
    sizes: &'a RefCell<Map<SizeKey, usize>>,
    /// The summary of each callee a call declared `inline` or small enough to take was weighed
    /// against, made the first time one is and kept for the round for the same reason as `sizes`.
    summaries: RefCell<Map<FuncId, summary::Summary>>,
}

/// What [`How::specialized_size`] keeps an answer under: the callee, how many values, instructions
/// and blocks it had made, the constants it was given in the order of their parameters, and
/// whether it weighed.
type SizeKey = (FuncId, (usize, usize, usize), Vec<(Value, Imm, Type)>, bool);

impl How<'_> {
    /// [`specialized_size`], worked out once for each body and set of constants.
    fn specialized_size(
        &self,
        module: &Module,
        callee: FuncId,
        values: &Map<Value, (Imm, Type)>,
        weighed: Option<&Interner>,
    ) -> usize {
        let mut passed: Vec<(Value, Imm, Type)> =
            values.iter().map(|(&param, &(imm, ty))| (param, imm, ty)).collect();
        passed.sort_unstable_by_key(|&(param, ..)| param);
        let counts = module[callee].counts();
        let key = (callee, (counts.values, counts.insts, counts.blocks), passed, weighed.is_some());
        if let Some(&size) = self.sizes.borrow().get(&key) {
            return size;
        }
        let size = specialized_size(&module[callee], values, weighed);
        self.sizes.borrow_mut().insert(key, size);
        size
    }

    /// [`folded_size`] for a call passing `values`, read off the callee's summary, and whether the
    /// constants removed anything. See [`summed_size`].
    fn copy_size(
        &self,
        module: &Module,
        callee: FuncId,
        values: Map<Value, (Imm, Type)>,
        weighed: Option<&Interner>,
    ) -> (usize, bool) {
        let mut summaries = self.summaries.borrow_mut();
        let summary = summaries
            .entry(callee)
            .or_insert_with(|| summary::Summary::of(&module[callee], self.names, None));
        summed_size(summary, &module[callee], values, weighed)
    }
}

/// How far inlining may grow a caller's frame, gcc's `large-stack-frame-growth` and
/// `large-stack-frame`. See `fits`.
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

    /// [`Self::CONSERVE`] when `conserve` says `-fconserve-stack` was given and [`Self::DEFAULT`]
    /// when not, with what `--param` set.
    #[must_use]
    pub fn new(conserve: bool) -> Self {
        if conserve {
            Self {
                percent: rucc_cost::param!(INLINE_FRAME_GROWTH_CONSERVE),
                bytes: rucc_cost::param!(INLINE_LARGE_FRAME_CONSERVE),
            }
        } else {
            Self {
                percent: rucc_cost::param!(INLINE_FRAME_GROWTH),
                bytes: rucc_cost::param!(INLINE_LARGE_FRAME),
            }
        }
    }
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
    // frame is measured against. Measured the first time a call asks or just before the first
    // splice, whichever comes first, which is the same answer as measuring here since nothing has
    // changed the caller yet. Most callers have no call this pass takes, and the measure runs the
    // scalar replacement's analysis over the whole of them. tamnd/rucc#3052.
    let mut own: Option<u64> = None;
    // The frame as it was last measured and every local a splice has copied in since, which is
    // never less than the frame it now has. See the bound in the loop below.
    let mut grown = 0;
    // The calls as the function was written. A call that arrives inside a body being inlined is
    // one the callee's own settling already had its chance at. A function that asked not to be
    // optimized is left with its calls, except for the ones that are a promise.
    let optnone = module[id].attrs.set.contains(AttrSet::OPTNONE);
    // A caller written `cold`, which in the kernel is every `__init` and `__exit` function, is one
    // whose calls gcc never thinks of as hot, and it inlines a call that is not hot only when that
    // does not make the program larger.
    let cold = module[id].attrs.set.contains(AttrSet::COLD);
    // A call `main` makes outside its loops is not hot to gcc either, since `main` runs once, and
    // neither is one from a function that never comes back. gcc 16 keeps the four calls `main`
    // makes to a `static` setter of ten lines at `-O2`, where the first pass used to copy it into
    // each. tamnd/rucc#3224. Found before anything is spliced in, as `deep` is below.
    let flat: Set<Inst> =
        if !cold && runs_once(&module[id], how.names) { flat(&module[id]) } else { Set::default() };
    // Whether a splice can leave a call through a pointer that [`resolved`] makes direct, which
    // takes an `always_inline` function to point at. Without one, walking the whole caller after
    // every splice to look for such a call finds nothing.
    let pointed = !optnone && how.wanted.values().any(|&(_, kind)| kind == Kind::Always);
    let mut calls: Vec<(Block, Inst, FuncId, Kind)> = {
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
                if kind == Kind::Asks && !passes_asked(func, inst, &module[callee]) {
                    return None;
                }
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
                kind == Kind::Once
                    && depth(block) > rucc_cost::param!(INLINE_CALLED_ONCE_LOOP_DEPTH)
            })
            .map(|&(_, inst, ..)| inst)
            .collect()
    } else {
        Set::default()
    };
    let mut stats = Stats::new();
    let mut spliced = false;
    let mut pool = Pool { on: how.share, results: Some(Results::default()), ..Pool::default() };
    let mut next = 0;
    while let Some(&(_, call, callee, kind)) = calls.get(next) {
        next += 1;
        if let Some(later) = how.later {
            later.borrow_mut().examined.insert((id, call));
        }
        let why = |failure: InlineFailure| match kind {
            Kind::Always => failure.why(),
            Kind::Hinted | Kind::Asks | Kind::Small | Kind::Auto => failure.hint(),
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
            Kind::Hinted | Kind::Asks => how.limit,
            Kind::Once => rucc_cost::param!(INLINE_CALLED_ONCE_INSNS) as usize,
            Kind::Small => 2 + module[id][module[id][call].args].len(),
            // gcc's limit is on the growth, the body less the call it replaces, and a growth as
            // large as the limit is refused.
            Kind::Auto => how.auto as usize + module[id][module[id][call].args].len(),
        };
        let (mut large, cut) = match kind {
            Kind::Asks => {
                (answered_size(&module[callee], passed(&module[id], call, &module[callee])), false)
            }
            Kind::Hinted => {
                how.copy_size(module, callee, passed(&module[id], call, &module[callee]), None)
            }
            Kind::Auto => how.copy_size(
                module,
                callee,
                passed(&module[id], call, &module[callee]),
                Some(how.names),
            ),
            Kind::Always | Kind::Once | Kind::Small => (size(&module[callee]), false),
        };
        // A call to a function that never comes back is one gcc predicts is never made, so it
        // is no more hot than one in a cold function. `machine_real_restart` in
        // arch/x86/kernel/reboot.c ends in an `ljmpl` objtool only accepts there, and gcc keeps
        // it a call in `native_machine_emergency_restart`.
        let cold_call = cold
            || flat.contains(&call)
            || module[callee].attrs.set.contains(AttrSet::COLD)
            || module[callee].attrs.set.contains(AttrSet::NORETURN);
        // The estimate above does not follow a constant through a block parameter or answer a
        // `__builtin_constant_p` about anything but a parameter, so where it would refuse, the
        // copy is made and cleaned up the way it would be once inlined, and that is measured.
        // Not for a body so far past the limit that no cleanup brings it under. See [`TRIED`].
        if matches!(kind, Kind::Hinted | Kind::Asks | Kind::Auto)
            && large <= most.saturating_mul(TRIED)
            && (large > most || cold_call && grows(&module[id], call, &module[callee], large, how))
        {
            let values = passed(&module[id], call, &module[callee]);
            let weighed = (kind == Kind::Auto).then_some(how.names);
            large = large.min(how.specialized_size(module, callee, &values, weighed));
        }
        if large > most {
            // Left for the second pass to weigh with its hints, which says why if it refuses too.
            if let (Some(later), Kind::Hinted | Kind::Auto) = (how.later, kind) {
                later.borrow_mut().deferred.insert((id, call));
                continue;
            }
            stats.missed(why(InlineFailure::TooLarge));
            continue;
        }
        if matches!(kind, Kind::Hinted | Kind::Asks | Kind::Auto)
            && cold_call
            && grows(&module[id], call, &module[callee], large, how)
        {
            stats.missed(why(InlineFailure::Unlikely));
            continue;
        }
        // The bound first, since `frame` runs the scalar replacement's analysis over the whole
        // caller and a caller that takes hundreds of calls would run it once for each of them. The
        // bound is never less than the frame, so a call that fits under it fits, and the frame is
        // only worked out again for the calls the bound cannot let through. That is how the heap
        // keeps its frames too, and on duktape.c at `-O2` measuring the frame for each call was
        // most of what this pass cost. tamnd/rucc#3052.
        let own = *own.get_or_insert_with(|| {
            grown = frame(&module[id], module.datalayout);
            grown
        });
        if kind != Kind::Always {
            let body = pool.growth(&module[callee], module.datalayout);
            if !fits(own, grown, body, how.growth) {
                grown = frame(&module[id], module.datalayout);
                if !fits(own, grown, body, how.growth) {
                    stats.missed(why(InlineFailure::Frame));
                    continue;
                }
            }
        }
        let mark = module[id].counts().insts;
        match splice(module, id, call, callee, how.convention, kind, &mut pool) {
            Ok(()) => {
                spliced = true;
                grown += made(&module[id], mark);
                // A pointer to an `always_inline` function handed to a body that calls through it
                // is a direct call once the body is in, and gcc inlines that one as well. The
                // kernel's `__inline_bsearch` is given `patch_cmp` that way in `poke_int3_handler`,
                // which is `noinstr`, and a call left out of line there is a call out of the
                // section objtool holds it to.
                if pointed {
                    calls.extend(resolved(module, id, how, false, None));
                }
                // A loop the callee said must stay a loop is in the caller now, and the pass that
                // would make it a call only knows functions. The whole caller keeps its loops,
                // which costs it the calls it would have had and never makes the wrong one.
                if module[callee].attrs.set.contains(AttrSet::NO_LOOP_IDIOM) {
                    module[id].attrs.set |= AttrSet::NO_LOOP_IDIOM;
                }
                stats.optimized(match kind {
                    Kind::Always => INLINED,
                    Kind::Hinted => HINT_INLINED,
                    // The calls were counted when the round began, so a function called from
                    // more than one place then is one the round takes into all its callers.
                    Kind::Once if how.calls.get(&module[callee].name).is_some_and(|&n| n > 1) => {
                        ALL_INLINED
                    }
                    Kind::Once => ONCE_INLINED,
                    Kind::Asks => ASKS_INLINED,
                    Kind::Small => SMALL_INLINED,
                    Kind::Auto => AUTO_INLINED,
                });
                if cut {
                    stats.note(CUT);
                }
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

/// The calls through a pointer that is the address of an `always_inline` function, made direct, or
/// of any function the step inlines when `every` says so, which the second pass does once a copy
/// has made the pointer one it knows. With `since` only the instructions made after that many
/// are looked at, which are the ones a copy brought.
///
/// What is called has to be what the call says it calls, so a function whose signature is not the
/// call's is left to be called through the pointer, the same as `crate::image` does.
fn resolved(
    module: &mut Module,
    id: FuncId,
    how: &How<'_>,
    every: bool,
    since: Option<usize>,
) -> Vec<(Block, Inst, FuncId, Kind)> {
    let mut out = Vec::new();
    let func = &module[id];
    let placed: Vec<(Block, Inst)> = match since {
        Some(mark) => (mark..func.counts().insts)
            .map(Inst::from_usize)
            .filter_map(|inst| func.block_of(inst).map(|block| (block, inst)))
            .collect(),
        None => func
            .blocks()
            .flat_map(|block| func.insts(block).map(move |inst| (block, inst)))
            .collect(),
    };
    let found: Vec<(Block, Inst, Symbol, FuncId, Kind)> = placed
        .into_iter()
        .filter_map(|(block, inst)| {
            let data = &func[inst];
            let Extra::Call(info) = data.extra else { return None };
            if data.opcode != Opcode::CallIndirect {
                return None;
            }
            let Def::Result { inst: made, .. } = func[*func[data.args].first()?].def else {
                return None;
            };
            if func[made].opcode != Opcode::GlobalAddr {
                return None;
            }
            let Extra::Symbol(name) = func[made].extra else { return None };
            let &(callee, kind) = how.wanted.get(&name)?;
            ((kind == Kind::Always || every)
                && module[callee].signature() == &func[func[info].signature])
                .then_some((block, inst, name, callee, kind))
        })
        .collect();
    let func = &mut module[id];
    for (block, inst, name, callee, kind) in found {
        let Extra::Call(info) = func[inst].extra else { continue };
        let args = func[func[inst].args][1..].to_vec();
        let args = func.push_values(&args);
        let mut call = func[info];
        call.callee = Some(name);
        let at = func.add_call(call);
        let data = &mut func[inst];
        data.opcode = Opcode::Call;
        data.args = args;
        data.extra = Extra::Call(at);
        out.push((block, inst, callee, kind));
    }
    out
}

/// The parameters of a body that it asks `__builtin_constant_p` about, by position.
///
/// The question may be about arithmetic on parameters rather than one of them, since what it asks
/// about is often a macro's argument. fs/super.c has `super_wake` ask, through `hweight32`, about
/// `flag & SUPER_WAKE_FLAGS`, and every caller passes a constant flag, so gcc inlines it and the
/// two warnings it checks the flag with fold away. Each parameter such a value is made of counts,
/// as long as the rest of it is constants.
fn asked(func: &Func) -> Vec<usize> {
    let Some(entry) = func.entry() else { return Vec::new() };
    let mut asked: Vec<usize> = Vec::new();
    for inst in func.blocks().flat_map(|block| func.insts(block)) {
        if func[inst].opcode != Opcode::IsConstant {
            continue;
        }
        let Some(&value) = func[func[inst].args].first() else { continue };
        let mut found = Vec::new();
        // Constants alone ask about nothing a caller passes, and fold without any help.
        if made_of_params(func, entry, value, ASKED_DEPTH, &mut found) {
            asked.extend(found);
        }
    }
    asked.sort_unstable();
    asked.dedup();
    asked
}

/// Whether that value is the entry block's parameters and constants put together by arithmetic
/// that folds, adding the positions of the parameters it reads to `found`, which stays empty for a
/// value of constants alone.
fn made_of_params(
    func: &Func,
    entry: Block,
    value: Value,
    depth: u32,
    found: &mut Vec<usize>,
) -> bool {
    match func[value].def {
        Def::Param { block, index } if block == entry => {
            usize::try_from(index).map(|at| found.push(at)).is_ok()
        }
        Def::Result { inst, .. } if depth > 0 => {
            let data = &func[inst];
            match data.opcode {
                Opcode::IConst => true,
                Opcode::And
                | Opcode::Or
                | Opcode::Xor
                | Opcode::Add
                | Opcode::Sub
                | Opcode::Mul
                | Opcode::Shl
                | Opcode::LShr
                | Opcode::AShr
                | Opcode::ICmp
                | Opcode::Trunc
                | Opcode::SExt
                | Opcode::ZExt => func[data.args]
                    .iter()
                    .all(|&arg| made_of_params(func, entry, arg, depth - 1, found)),
                _ => false,
            }
        }
        _ => false,
    }
}

/// How many instructions a body has that are still work once the parameters it asks
/// `__builtin_constant_p` about are constants.
///
/// That is what an inlined copy comes to after folding, and it is the size gcc's inliner weighs
/// such a call by. Without it the arithmetic a `BUILD_BUG_ON` checks the constant with counts
/// against the body, and in the kernel that is most of a body that asks, so a function the size
/// of a hinted one looks two or three times larger than it is.
fn answered_size(func: &Func, values: Map<Value, (Imm, Type)>) -> usize {
    let Some(entry) = func.entry() else { return size(func) };
    let asked = asked(func);
    let known = func[entry]
        .params
        .iter()
        .enumerate()
        .filter(|(at, _)| asked.contains(at))
        .map(|(_, &value)| value)
        .collect();
    folded_size(func, known, values, None)
}

/// The callee's parameters this call passes a constant for, with the constant, which is what
/// gcc's estimate of an inlined copy knows about the call it stands for. `kzalloc` called with a
/// `sizeof` takes the one arm of `kmalloc` that picks a size class, and weighed without it the
/// body is several times the limit.
fn passed(func: &Func, call: Inst, callee: &Func) -> Map<Value, (Imm, Type)> {
    let Some(entry) = callee.entry() else { return Map::default() };
    let args = &func[func[call].args];
    callee[entry]
        .params
        .iter()
        .zip(args)
        .filter_map(|(&param, &arg)| {
            let found = crate::fold::evaluated(func, arg, ASKED_DEPTH)?;
            Some((param, found))
        })
        .collect()
}

/// How many instructions a copy of a body has that are still work once the constants the call
/// passes are in it and the passes that run right after inlining have folded it.
///
/// What gcc weighs a call by is the body as the early passes left it, with what the call site
/// knows applied on top, and a `__builtin_constant_p` it cannot answer yet counts as the arm that
/// is taken once the answer is no. `efi_enabled` asks it about a bit number and then about the
/// address of a field, and only a real fold sees the second one come out true.
fn specialized_size(
    callee: &Func,
    values: &Map<Value, (Imm, Type)>,
    weighed: Option<&Interner>,
) -> usize {
    use crate::Pass;
    let mut copy = callee.clone();
    let Some(first) = copy.entry().and_then(|entry| copy.insts(entry).next()) else {
        return size(callee);
    };
    let mut forward = Map::default();
    for (&param, &(value, ty)) in values {
        let imm = copy.add_imm(value);
        let data = InstData { extra: Extra::Imm(imm), ..InstData::new(Opcode::IConst) };
        let span = copy.span(first);
        let inst = copy.create_inst(data, &[ty], span);
        copy.insert_before(inst, first);
        if let Some(result) = copy[inst].results().next() {
            forward.insert(param, result);
        }
    }
    crate::uses::substitute(&mut copy, &forward);
    let mut an = crate::Analyses::new(crate::machine::Machine::unknown());
    let mut fuel = crate::Fuel::unlimited();
    // `reassoc` stands for gcc's `forwprop`, which puts the constants of a chain of adds together
    // before the early inliner weighs the body, so sixteen lines adding a constant each are one
    // add to gcc. Without it a helper like that grows `main` by fourteen and stays a call there,
    // where gcc copies it (tamnd/rucc#3224).
    let passes: [&dyn Pass; 7] = [
        &crate::fold::Fold,
        &crate::simplify::Simplify,
        &crate::reassoc::Reassoc,
        &crate::constant_p::ConstantP,
        &crate::sccp::Sccp,
        &crate::simplify_cfg::SimplifyCfg,
        &crate::dce::Dce,
    ];
    for _ in 0..2 {
        for pass in passes {
            // The manager's rule, so a pass that changed nothing leaves the next one the graph
            // it was given rather than one to build again, and one that changed something keeps
            // what it says it kept and what the graph read again says it did not move.
            if pass.run(&mut copy, &mut an, &mut fuel).changed() {
                let keeps = an.unmoved(&copy, pass.preserves());
                an.settle(&copy, keeps, false);
            }
        }
    }
    folded_size(&copy, Set::default(), Map::default(), weighed)
}

/// How many instructions a body has that are still work once what only depends on constants and
/// on the values in `known` has folded away.
///
/// gcc weighs a callee declared `inline` by its body after the early passes cleaned it up, and
/// this pass runs before any of that. A `this_cpu_read` in a body is a switch over four sizes with
/// a load in each arm and a `do { } while (0)` around it when this pass sees it, and one load once
/// the size is folded, so counting it as written made `alloc_pages_node` in the kernel three times
/// the limit where gcc inlines it everywhere.
///
/// What is known with its number in `values` goes further: a branch on it takes one arm, and the
/// arms it does not take are not counted at all, as they are gone from the copy once it folds.
fn folded_size(
    func: &Func,
    known: Set<Value>,
    values: Map<Value, (Imm, Type)>,
    weighed: Option<&Interner>,
) -> usize {
    folded(func, known, values, weighed, None).0
}

/// Who reads each value and as which operand, the argument of a jump to a block being read by a
/// jump at no operand.
///
/// The lists are laid end to end by value number, counted first and filled after. A list for each
/// value in a map was an allocation for each value of every body the inliner weighs, and it weighs
/// thousands. tamnd/rucc#3052.
struct Readers {
    /// Where the readers of each value start, with one more on the end for where the last stop.
    start: Vec<u32>,
    /// The readers, in the order the blocks have them.
    list: Vec<(Opcode, usize)>,
}

impl Readers {
    fn of(func: &Func) -> Self {
        let mut start = vec![0u32; func.counts().values + 1];
        Self::walk(func, |arg, _| start[arg.index() + 1] += 1);
        for index in 1..start.len() {
            start[index] += start[index - 1];
        }
        let mut next = start.clone();
        let mut list = vec![(Opcode::Jump, 0); start[start.len() - 1] as usize];
        Self::walk(func, |arg, read| {
            list[next[arg.index()] as usize] = read;
            next[arg.index()] += 1;
        });
        Self { start, list }
    }

    /// Every read of a value in the blocks of a body, with the opcode reading it and which operand
    /// it is.
    fn walk(func: &Func, mut each: impl FnMut(Value, (Opcode, usize))) {
        for inst in func.blocks().flat_map(|block| func.insts(block)) {
            for (at, &arg) in func[func[inst].args].iter().enumerate() {
                each(arg, (func[inst].opcode, at));
            }
            for call in func.successors(inst) {
                for &arg in &func[call.args] {
                    each(arg, (Opcode::Jump, usize::MAX));
                }
            }
        }
    }

    fn of_value(&self, value: Value) -> &[(Opcode, usize)] {
        &self.list[self.start[value.index()] as usize..self.start[value.index() + 1] as usize]
    }

    /// Whether anything reads the value.
    fn read(&self, value: Value) -> bool {
        !self.of_value(value).is_empty()
    }

    /// Whether the value is read and every reader is one `fits` takes. A value nothing reads is
    /// not one whose readers all fit.
    fn only(&self, value: Option<Value>, fits: &dyn Fn(Opcode, usize) -> bool) -> bool {
        value.is_some_and(|value| {
            let readers = self.of_value(value);
            !readers.is_empty() && readers.iter().all(|&(opcode, at)| fits(opcode, at))
        })
    }
}

/// What [`folded_size`] counts, with how long it takes as well, which is each instruction counted
/// weighed by how often `frequency` says its block runs.
fn folded(
    func: &Func,
    mut known: Set<Value>,
    mut values: Map<Value, (Imm, Type)>,
    weighed: Option<&Interner>,
    frequency: Option<&Map<Block, f64>>,
) -> (usize, f64) {
    known.extend(values.keys().copied());
    // Who reads each value and as which operand, for what is part of the instruction reading it
    // once there is code: an address a load or a store takes, the index it scales, and the
    // comparison a branch tests.
    let readers = Readers::of(func);
    let only =
        |value: Option<Value>, fits: &dyn Fn(Opcode, usize) -> bool| readers.only(value, fits);
    let address =
        |opcode: Opcode, at: usize| matches!((opcode, at), (Opcode::Load, 0) | (Opcode::Store, 1));
    let cfg = Cfg::new(func);
    let mut live: Set<Block> = cfg.entry().into_iter().collect();
    let mut work = 0;
    let mut time = 0.0;
    for block in cfg.reverse_postorder() {
        if !live.contains(&block) {
            continue;
        }
        for inst in func.insts(block) {
            let data = &func[inst];
            let args = &func[data.args];
            let folds = args.iter().all(|arg| known.contains(arg));
            if let (true, Some(result)) = (folds, data.results().next()) {
                let operand = |arg: Value| {
                    values.get(&arg).copied().or_else(|| crate::fold::constant(func, arg))
                };
                if let Some(found) = computed(func, inst, &operand) {
                    values.insert(result, found);
                }
            }
            if func.is_terminator(inst) {
                let decided = args
                    .first()
                    .and_then(|arg| values.get(arg))
                    .and_then(|&(value, _)| goes_to(func, data, value));
                match decided {
                    Some(call) => {
                        live.insert(call.block);
                    }
                    None => live.extend(func.successors(inst).map(|call| call.block)),
                }
            }
            let free = match data.opcode {
                // A jump is gone once the blocks either side of it are one, and a hint that a
                // block cannot be reached is no code at all. The address of a global is an
                // operand of whatever uses it.
                Opcode::IConst
                | Opcode::FConst
                | Opcode::GlobalAddr
                | Opcode::IsConstant
                | Opcode::Jump
                | Opcode::UnreachableHint => true,
                Opcode::Shl
                | Opcode::LShr
                | Opcode::AShr
                | Opcode::Add
                | Opcode::Sub
                | Opcode::Mul
                | Opcode::And
                | Opcode::Or
                | Opcode::Xor
                | Opcode::Ctlz
                | Opcode::Cttz
                | Opcode::Ctpop
                | Opcode::ICmp
                | Opcode::Select => folds,
                Opcode::BrIf | Opcode::Switch => {
                    args.first().is_some_and(|arg| known.contains(arg))
                }
                _ => false,
            };
            if free {
                known.extend(data.results());
                continue;
            }
            // What costs nothing in the copy without being a constant: a conversion between
            // registers, a constant offset the memory access takes as part of its address, a local
            // of a fixed size, the return, which becomes a jump to where the call was, and a
            // `__builtin_expect`, which is only a weight on the branch. gcc counts these as nothing
            // too, the return as eliminated by inlining.
            let costless = match data.opcode {
                Opcode::Trunc
                | Opcode::SExt
                | Opcode::ZExt
                | Opcode::PtrToInt
                | Opcode::IntToPtr
                | Opcode::Bitcast => {
                    if folds {
                        known.extend(data.results());
                    }
                    true
                }
                Opcode::PtrAdd => {
                    args.get(1).is_some_and(|arg| known.contains(arg))
                        || only(data.results().next(), &address)
                }
                Opcode::Mul | Opcode::Shl => {
                    args.get(1).is_some_and(|arg| known.contains(arg))
                        && only(data.results().next(), &|opcode, at| {
                            opcode == Opcode::PtrAdd && at == 1
                        })
                }
                Opcode::ICmp => only(data.results().next(), &|opcode, at| {
                    matches!((opcode, at), (Opcode::BrIf | Opcode::Expect, 0))
                }),
                Opcode::Expect => true,
                Opcode::Alloca => args.is_empty(),
                Opcode::Return | Opcode::LifetimeEnd => true,
                _ => false,
            };
            if !costless {
                let read = data.results().any(|result| readers.read(result));
                let cost = weighed.map_or(1, |names| weight(func, inst, names, read));
                work += cost;
                let often = frequency.and_then(|it| it.get(&block)).copied().unwrap_or(1.0);
                // A call takes longer than its size says, which is gcc's `eni_time_weights`.
                let waits = match data.opcode {
                    Opcode::Call | Opcode::CallIndirect | Opcode::TailCall => {
                        f64::from(rucc_cost::param!(INLINE_CALL_TIME)) - 1.0
                    }
                    _ => 0.0,
                };
                time += (cost as f64 + waits) * often;
            }
        }
    }
    (work, time)
}

/// What an instruction works out to when it has one result and `operand` gives the numbers of what
/// it reads. A `__builtin_constant_p` whose operand is known is one.
fn computed(
    func: &Func,
    inst: Inst,
    operand: &dyn Fn(Value) -> Option<(Imm, Type)>,
) -> Option<(Imm, Type)> {
    let data = &func[inst];
    let result = data.results().next().filter(|_| data.results == 1)?;
    let ty = func[result].ty;
    let found = if data.opcode == Opcode::IsConstant {
        Some(Imm::int(1, ty))
    } else if ty.is_int() && ty.is_scalar() {
        crate::fold::arithmetic(data, &func[data.args], ty, operand)
    } else {
        None
    };
    found.map(|found| (found, ty))
}

/// Where a `br_if` or a `switch` goes when what it tests is `value`.
fn goes_to(func: &Func, data: &InstData, value: Imm) -> Option<BlockCall> {
    match data.extra {
        Extra::Targets(targets) if data.opcode == Opcode::BrIf => {
            func[targets].get(usize::from(value.bits() == 0)).copied()
        }
        Extra::Switch(at) if data.opcode == Opcode::Switch => {
            let info = func[at];
            let case = func[info.cases].iter().position(|it| *it == value);
            func[info.targets].get(case.map_or(0, |case| case + 1)).copied()
        }
        _ => None,
    }
}

/// [`folded_size`] with nothing known but `values`, read off the callee's summary when it has an
/// exact one, and whether the constants removed anything.
///
/// Every debug build checks the summary against the walk, so a rule changed in one and not the
/// other is found by the first test that weighs a call.
fn summed_size(
    summary: &summary::Summary,
    callee: &Func,
    values: Map<Value, (Imm, Type)>,
    weighed: Option<&Interner>,
) -> (usize, bool) {
    let Some(estimate) = summary.estimate(callee, &values) else {
        return (folded_size(callee, Set::default(), values, weighed), false);
    };
    let size = if weighed.is_some() { estimate.weighed } else { estimate.plain };
    debug_assert_eq!(
        size,
        folded_size(callee, Set::default(), values, weighed),
        "the summary of {:?} is not the walk",
        callee.name
    );
    (size, estimate.cut)
}

/// How long a copy of the callee takes when the call passes `values`, read off a summary made with
/// `frequency` when it is exact, and walked as [`folded`] does when not.
fn summed_time(
    summary: &summary::Summary,
    callee: &Func,
    values: Map<Value, (Imm, Type)>,
    names: &Interner,
    frequency: &Map<Block, f64>,
) -> f64 {
    let Some(estimate) = summary.estimate(callee, &values) else {
        return folded(callee, Set::default(), values, Some(names), Some(frequency)).1;
    };
    if cfg!(debug_assertions) {
        let walked = folded(callee, Set::default(), values, Some(names), Some(frequency)).1;
        let near = (estimate.time - walked).abs() <= 1e-9 * walked.abs().max(1.0);
        assert!(
            near,
            "the summary of {:?} takes {} and the walk {walked}",
            callee.name, estimate.time
        );
    }
    estimate.time
}

/// What gcc's `estimate_num_insns` charges for an instruction when it weighs a body by size, for
/// the kinds where that is not one.
///
/// A call is one, or three through a pointer, and one more for each argument passed and for the
/// result when something reads it, since each of those is a move to gcc. `call_netdevice_notifiers_info`
/// in net/core/dev.c is three calls, a load and a test, which is twenty eight to gcc and stays a
/// call in its thirty callers. A switch is two for each label, the default included and a run of cases that go to the same
/// place counted once, since gcc expects a compare and a branch for each. An `asm` is one for each
/// line of its template, a `;` ending a line as much as a newline does, and one at most when it is
/// written `asm inline`, which is what the kernel writes for the ones that only add to a section.
/// `nl80211_chan_width_to_mhz` in net/wireless/chan.c is a switch of ten labels and two such
/// annotations, which is thirty one to gcc and too large to copy without being asked. A
/// conditional branch is two, as a `GIMPLE_COND` holds its compare and gcc charges the compare on
/// top of the branch, whether or not the compare is also read elsewhere. `classify` in the corpus
/// case `control-flow.many-returns` is four tests of its argument, eleven to gcc with the call it
/// goes with, and stays a call in its five callers at `-O1`, where counted one apiece it was copied
/// into all of them (tamnd/rucc#3379). A branch on a block's parameter is one, as that is how
/// `a && b` comes out of the front end, a join of the two tests where gcc branches on the second
/// test straight away. The merge in the sorter of SQLite is a loop on `a && b` and comes to thirty
/// three that way, as it does to gcc, where with the join at two it was over the limit wasm holds a
/// call in a loop to.
fn weight(func: &Func, inst: Inst, names: &Interner, read: bool) -> usize {
    let data = &func[inst];
    match (data.opcode, data.extra) {
        (Opcode::Call | Opcode::CallIndirect | Opcode::TailCall, Extra::Call(at)) => {
            let through = data.opcode == Opcode::CallIndirect
                || (data.opcode == Opcode::TailCall && func[at].callee.is_none());
            let passed = func[data.args].len() - usize::from(through);
            let call = if through { 3 } else { 1 };
            call + passed + usize::from(read)
        }
        (Opcode::BrIf, _) => {
            let joined = func[data.args]
                .first()
                .is_some_and(|&test| matches!(func[test].def, Def::Param { .. }));
            if joined { 1 } else { 2 }
        }
        (Opcode::Switch, Extra::Switch(at)) => {
            let info = func[at];
            let targets = &func[info.targets];
            let mut cases: Vec<(u128, Block)> = func[info.cases]
                .iter()
                .zip(targets.iter().skip(1))
                .map(|(value, target)| (value.bits(), target.block))
                .collect();
            cases.sort_unstable();
            let runs = cases
                .iter()
                .enumerate()
                .filter(|&(at, &(value, block))| {
                    at == 0 || cases[at - 1] != (value.wrapping_sub(1), block)
                })
                .count();
            2 * (runs + 1)
        }
        (Opcode::InlineAsm, Extra::Asm(at)) => {
            let template = names.resolve(func[at].template);
            let lines = if template.is_empty() {
                0
            } else {
                1 + template.chars().filter(|&c| c == '\n' || c == ';').count()
            };
            let lines = if data.flags.contains(Flags::INLINE) { lines.min(1) } else { lines };
            lines.max(1)
        }
        _ => 1,
    }
}

/// How many times the limit a body the estimate weighs may be and still be copied and cleaned up to
/// see whether the constants a call passes fold it under.
///
/// The most a cleanup took off the estimate on the corpus was nine tenths, for
/// `is_rfc3986_uri_char` in libexpat's `xmlparse.c`, a switch over the characters a URI may hold
/// that goes from 172 to 18. A body further past the limit than this is a large one no call ever
/// fits, and on lz4hc.c at `-O2` copying and cleaning up `LZ4HC_compress_generic` and the bodies
/// around it, each a hundred times the limit or more, was an eighth of the instructions of the
/// build. tamnd/rucc#3052.
const TRIED: usize = 16;

/// How far [`passes_asked`] looks through arithmetic over constants for an argument that works out
/// to one. A mask the kernel writes with `GENMASK` is a dozen shifts, ands and subtractions of
/// constants when this pass runs, since the folding comes after it.
const ASKED_DEPTH: u32 = 32;

/// Whether this call passes a constant for one of the parameters the callee asks about.
fn passes_asked(func: &Func, call: Inst, callee: &Func) -> bool {
    let args = &func[func[call].args];
    asked(callee).into_iter().any(|at| {
        args.get(at).is_some_and(|&arg| match func[arg].def {
            Def::Result { inst, .. } => {
                matches!(func[inst].opcode, Opcode::IConst | Opcode::FConst)
                    || crate::fold::evaluated(func, arg, ASKED_DEPTH).is_some()
            }
            _ => false,
        })
    })
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
///
/// A local that scalar replacement makes values of is not in the pool. gcc shares the bytes of
/// locals when it expands to RTL, after its scalar replacement, so its sharing never keeps a value
/// in memory. A shared slot is one object to scalar replacement, so one splice that gives the
/// address of its local to a call kept the locals of every other splice in the slot in memory as
/// well. In SQLite, the `u32` that `sqlite3Get4byte` copies the bytes into shared one slot with
/// the `int` of other bodies in `sqlite3VdbeExec`, and each read of a page header went through
/// the stack.
#[derive(Debug, Default)]
struct Pool {
    /// Whether anything is shared at all, which is `-fstack-reuse=` and off at `-O0`.
    on: bool,
    /// Which splice this is, counting from one.
    site: u32,
    /// The slots so far, each with its size and the last splice that took it.
    slots: Vec<(Inst, u64, u32)>,
    /// The largest clique the caller's accesses name, worked out at the first splice and kept up
    /// to date by each one after it, which only adds accesses. Working it out again for each splice
    /// walked the whole caller each time, and blake2b.c at `-O2` inlines `rotr64` into one body
    /// close to four hundred times. Whoever edits the caller between splices empties it.
    highest: Option<u16>,
    /// Who reads the results of the caller's calls, for a caller that nothing but its splices
    /// edits. Without it a splice points the readers of the call's results at the block after
    /// the copy by rewriting every run of operands the caller has.
    results: Option<Results>,
}

/// The instructions that read the result of each call in a caller, kept up to date across the
/// splices into it.
///
/// A splice points the readers of the call's results at the parameters of the block after the
/// copy. Rewriting every run of operands to do that is a walk over all of the caller, and on
/// quickjs.c at `-O2` the first pass splices into `JS_CallInternal` and its neighbours hundreds of
/// times each, which was a fifth of the compile. The instructions are filed once and then only
/// the ones each splice made are, which is everything that can come to read a call's result: a
/// splice adds instructions and takes some out, and the one other edit, [`resolved`], gives a call
/// a run of operands it already had less the pointer. An instruction removed since it was filed
/// stays on the lists, and is skipped.
#[derive(Debug, Default)]
struct Results {
    /// By value, for the values a call makes.
    of: Map<Value, Vec<Inst>>,
    /// How many instructions the caller had the last time this looked.
    seen: usize,
}

impl Results {
    /// Files the instructions made since the last look, which the first time is all of them.
    fn catch_up(&mut self, func: &Func) {
        let count = func.counts().insts;
        for inst in (self.seen..count).map(Inst::from_usize) {
            if func.block_of(inst).is_none() {
                continue;
            }
            let runs =
                std::iter::once(func[inst].args).chain(func.successors(inst).map(|to| to.args));
            for run in runs {
                for &value in &func[run] {
                    let Def::Result { inst: def, .. } = func[value].def else { continue };
                    if matches!(func[def].extra, Extra::Call(_)) {
                        self.of.entry(value).or_default().push(inst);
                    }
                }
            }
        }
        self.seen = count;
    }

    /// [`crate::uses::substitute_all`] over the instructions that read the values in the map.
    fn substitute(&mut self, func: &mut Func, forward: &Map<Value, Value>) {
        self.catch_up(func);
        let with = |value: Value| crate::uses::chase(forward, value);
        let mut runs = Vec::new();
        for from in forward.keys() {
            for inst in self.of.remove(from).unwrap_or_default() {
                if func.block_of(inst).is_none() {
                    continue;
                }
                runs.clear();
                runs.push(func[inst].args);
                runs.extend(func.successors(inst).map(|to| to.args));
                for &run in &runs {
                    func.rewrite(run, with);
                }
            }
        }
        crate::uses::rename(func, forward);
    }
}

impl Pool {
    /// A slot for the callee's `alloca` whose memory is `extra` to take over, when there is one.
    fn take(
        &mut self,
        func: &mut Func,
        callee: &Func,
        extra: Extra,
        flags: Flags,
    ) -> Option<Value> {
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
        // A slot that holds a local wanting a canary wants one whichever local is in it.
        func[*inst].flags |= flags.intersection(Flags::GUARD);
        func[*inst].first_result
    }

    /// How many bytes splicing `callee` in would add to the caller's frame, which is its [`frame`]
    /// less the slots it would take over.
    fn growth(&self, callee: &Func, layout: DataLayout) -> u64 {
        let mut free: Vec<u64> = if self.on {
            self.slots.iter().map(|&(_, size, _)| size).collect()
        } else {
            Vec::new()
        };
        let mut grows = 0;
        let unread = gone(callee, layout);
        for inst in callee.blocks().flat_map(|block| callee.insts(block)) {
            if callee[inst].opcode != Opcode::Alloca || !callee[inst].args.is_empty() {
                continue;
            }
            if callee[inst].first_result.is_some_and(|value| unread.contains(&value)) {
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

/// Whether copying a body of `size` instructions in place of `call` makes the program larger by
/// more than gcc lets a call that is not hot grow it.
///
/// The copy costs the body less the call it replaces, a call being one instruction and one more
/// for each argument. gcc's early inliner takes a copy that grows the caller by no more than
/// [`INLINE_EARLY_INSNS`], or [`INLINE_EARLY_INSNS_O3`] at `-O3`, before it has worked out which functions are cold, holding a body that
/// makes calls of its own to that for each of them and itself together. What it leaves, the later
/// inliner takes into a caller that is not hot only when the program does not grow, which is
/// `growth_positive_p`. A `static` body nothing reaches but its calls goes away once every call
/// has its copy, so what it costs then is a copy for every call less the body it no longer needs.
fn grows(func: &Func, call: Inst, callee: &Func, size: usize, how: &How<'_>) -> bool {
    let cost = 1 + func[func[call].args].len();
    let growth = size.saturating_sub(cost);
    let calls = callee
        .blocks()
        .flat_map(|block| callee.insts(block))
        .filter(|&inst| {
            matches!(callee[inst].opcode, Opcode::Call | Opcode::CallIndirect | Opcode::TailCall)
        })
        .count();
    if growth * (calls + 1) <= how.early as usize {
        return false;
    }
    let removable = callee.linkage == Linkage::Internal
        && !callee.attrs.set.contains(AttrSet::USED)
        && !how.elsewhere.contains(&callee.name);
    // The calls that are hot are taken first and go in when they fit, so what the copy left out of
    // line has to pay for is the calls that are not, unless the callee is cold itself and every
    // call to it is weighed this way.
    let sites = if callee.attrs.set.contains(AttrSet::COLD) { how.calls } else { how.cold };
    let sites = sites.get(&callee.name).copied().unwrap_or(1).max(1);
    !removable || growth * sites > size
}

/// Whether a body could be no larger than a call to it, which is the call, one instruction for each
/// argument and the use of what comes back. A body that returns a constant is two instructions.
fn small(func: &Func) -> bool {
    size(func) <= 2 + func.signature().params.len()
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
fn frame(func: &Func, layout: DataLayout) -> u64 {
    let local = |&inst: &Inst| func[inst].opcode == Opcode::Alloca && func[inst].args.is_empty();
    let mut locals = func.blocks().flat_map(|block| func.insts(block)).filter(local).peekable();
    // A body with no local in memory has no frame, and finding which of its locals are gone was
    // two more walks over it and a graph for nothing. tamnd/rucc#3052.
    if locals.peek().is_none() {
        return 0;
    }
    let unread = gone(func, layout);
    locals
        .filter(|&inst| !func[inst].first_result.is_some_and(|value| unread.contains(&value)))
        .filter_map(|inst| match func[inst].extra {
            Extra::Mem(mem) => Some(func[mem].size),
            _ => None,
        })
        .sum()
}

/// Every byte of the locals made in a body since it had `mark` instructions, the ones [`frame`]
/// would leave out as gone included. A splice adds to the frame only the locals it copies in, so
/// the frame before it and this are never less than the frame after it.
fn made(func: &Func, mark: usize) -> u64 {
    (mark..func.counts().insts)
        .map(Inst::from_usize)
        .filter(|&inst| func.block_of(inst).is_some())
        .filter(|&inst| func[inst].opcode == Opcode::Alloca && func[inst].args.is_empty())
        .filter_map(|inst| match func[inst].extra {
            Extra::Mem(mem) => Some(func[mem].size),
            _ => None,
        })
        .sum()
}

/// The locals gcc no longer has in memory by the time it measures a frame, which are the ones
/// nothing reads and the ones its early scalar replacement made values of.
///
/// The second kind is what the kernel's tracepoints are made of. `__do_trace_contention_begin` in
/// `<trace/events/lock.h>` holds a `guard(srcu_fast_notrace)`, a structure whose address only
/// goes to a destructor that is inlined into it, and gcc's frame for it has nothing in it. Counted,
/// those bytes put `__mutex_lock_common` past the 100 bytes `-fconserve-stack` allows, and the six
/// tracepoints in it stayed calls with their static keys out of line.
fn gone(func: &Func, layout: DataLayout) -> Set<Value> {
    let mut out = unread(func);
    out.extend(crate::sroa::scalarizable(func, layout));
    out
}

/// The locals nothing ever reads, which gcc has deleted by the time it measures a frame.
///
/// A local whose address is only written through, marked dead, or compared is one gcc's early
/// passes fold the compare of and then remove. That is the pair `typecheck()` declares only to
/// write `(void)(&__dummy == &__dummy2)`, and under `-ftrivial-auto-var-init=zero` each of them
/// is also cleared. Counted, they are 16 bytes that push `io_handle_query_entry` past the 100
/// bytes `-fconserve-stack` allows, and gcc inlines it and settles a `WARN_ON_ONCE` with the
/// object size it then knows.
fn unread(func: &Func) -> Set<Value> {
    let mut locals: Set<Value> = func
        .blocks()
        .flat_map(|block| func.insts(block))
        .filter(|&inst| func[inst].opcode == Opcode::Alloca && func[inst].args.is_empty())
        .filter_map(|inst| func[inst].first_result)
        .collect();
    if locals.is_empty() {
        return locals;
    }
    for inst in func.blocks().flat_map(|block| func.insts(block)) {
        let data = &func[inst];
        for (at, arg) in func[data.args].iter().enumerate() {
            let harmless = match data.opcode {
                Opcode::Store => at == 1,
                Opcode::Memset => at == 0,
                Opcode::LifetimeEnd | Opcode::ICmp => true,
                _ => false,
            };
            if !harmless {
                locals.remove(arg);
            }
        }
        // A local handed to another block is read there under another name, which is how a
        // pointer that walks a buffer starts out. `asyncQueueProcessPageEntries` in Postgres's
        // `async.c` does nothing else with its 8192 byte page, and leaving it out of the frame let
        // the page into `asyncQueueReadAllNotifications`, whose own frame gcc keeps at 80 bytes.
        for call in func.successors(inst) {
            for arg in &func[call.args] {
                locals.remove(arg);
            }
        }
    }
    locals
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
    let mut copies = std::mem::take(&mut module.copies);
    let scalar = crate::sroa::scalarizable(&module[callee], module.datalayout);
    let result = check(&func, call, &module[callee], convention, kind).map(|plan| {
        copy(&mut func, call, &module[callee], &plan, pool, &scalar, &mut copies);
    });
    module[caller] = func;
    module.copies = copies;
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

/// What [`moved`] needs to know about the copy it is moving spans into.
struct Moving {
    /// Where the call being replaced is.
    call: Span,
    /// The copy, or `None` when copies are not told apart and the body keeps its spans.
    site: Option<usize>,
    /// The copy made here of each copy that was already in the callee, by which one it was.
    inner: Map<usize, Option<usize>>,
}

/// Where a span of the callee's is in the copy of it.
///
/// A span of a statement in the body is moved into the copy's own positions. A span in a copy
/// that was already in the callee, which is a body inlined into it before, is moved into a copy
/// of that copy, made once for each one and inside this copy, so that the nesting of the calls
/// is kept. What is left is the prologue, which says the call, as it always did.
fn moved(copies: &mut Copies, moving: &mut Moving, callee: &Func, span: Span) -> Span {
    let body = callee.declared;
    if span.is_dummy() || body.is_dummy() {
        return span;
    }
    if span != body && body.contains(span.lo) {
        return match moving.site {
            Some(site) => copies.shift(site, body.lo, span),
            None => span,
        };
    }
    if let Some(was) = copies.site_at(span.lo).filter(|_| moving.site.is_some()) {
        let made = match moving.inner.get(&was) {
            Some(&made) => made,
            None => {
                let old = copies.sites()[was];
                let call = moved(copies, moving, callee, old.call);
                let made = copies.make(old.of, call, old.callee);
                moving.inner.insert(was, made);
                made
            }
        };
        if let Some(made) = made {
            return copies.shift(made, copies.sites()[was].at, span);
        }
    }
    moving.call
}

/// Splices the callee in where the call is, which [`check`] has said it can be. The locals in
/// `scalar` are ones that scalar replacement makes values of, which are not put in the [`Pool`].
fn copy(
    func: &mut Func,
    call: Inst,
    callee: &Func,
    plan: &Plan,
    pool: &mut Pool,
    scalar: &Set<Value>,
    copies: &mut Copies,
) {
    let block = func.block_of(call).expect("a call being inlined is in a block");
    let entry = func.entry().expect("a function with a call in it has a body");
    // Taken before anything of the callee's is in the caller, whose payloads until they are mapped
    // below are the callee's numbers and would be read as the caller's.
    let past = *pool.highest.get_or_insert_with(|| highest_clique(func));
    let mark = func.counts().insts;
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
    func.move_after(call, after);
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
    let constants: Vec<Option<(Imm, Type)>> =
        passed.iter().map(|&arg| crate::fold::constant(func, arg)).collect();
    let (live, decided) = reached(callee, start, &constants);
    let mut blocks = Map::default();
    let mut values = Map::default();
    for from in callee.blocks() {
        if !live[from.index()] {
            continue;
        }
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
    // every span it has. When the driver asked for copies to be told apart, the place is in the
    // copy rather than in the body. See `moved`.
    let mut moving = Moving { call: func.span(call), site: None, inner: Map::default() };
    moving.site = copies.make(callee.declared, moving.call, callee.name);
    // A `musttail` call in the callee is in tail position there and is not here, unless the call
    // being replaced was one too, which is where gcc keeps the promise as well.
    let dropped =
        if func[call].flags.contains(Flags::MUST_TAIL) { Flags::NONE } else { Flags::MUST_TAIL };
    // The callee's locals whose lifetimes the safety lowering marked where they begin. In the
    // callee each one ended with the frame, which was what said a pointer to it had outlived it,
    // and here the frame is the caller's and goes on. So each one ends where the callee returned
    // as well, which is where its witness is shut. Nothing is marked when the build is not
    // instrumented, so this is nothing then.
    let mut begun: Vec<Value> = Vec::new();
    for inst in callee.blocks().flat_map(|block| callee.insts(block)) {
        if callee[inst].opcode != Opcode::MetaBegin {
            continue;
        }
        let Some(&slot) = callee[callee[inst].args].first() else { continue };
        let Def::Result { inst: def, .. } = callee[slot].def else { continue };
        let fixed = callee[def].opcode == Opcode::Alloca && callee[def].args.is_empty();
        if fixed && !begun.contains(&slot) {
            begun.push(slot);
        }
    }
    let mut exits = Vec::new();
    let mut made = Vec::new();
    pool.site += 1;
    for from in callee.blocks() {
        if !live[from.index()] {
            continue;
        }
        for inst in callee.insts(from) {
            let data = &callee[inst];
            if data.opcode == Opcode::VaArgPack {
                continue;
            }
            let opcode = match data.opcode {
                Opcode::Return => Opcode::Jump,
                _ if decided.contains_key(&inst) => Opcode::Jump,
                Opcode::VaArgPackLen => Opcode::IConst,
                opcode => opcode,
            };
            let types: Vec<Type> = data.results().map(|value| callee[value].ty).collect();
            // A `return` that becomes a jump keeps nothing, since the flags it carries are about
            // leaving the function, and this one no longer does.
            let flags = data.flags.without(dropped).intersection(Flags::legal_on(opcode));
            let shell = InstData { flags, ..InstData::new(opcode) };
            let fixed = opcode == Opcode::Alloca && data.args.is_empty();
            let shared = fixed && !data.first_result.is_some_and(|value| scalar.contains(&value));
            let taken = if shared { pool.take(func, callee, data.extra, data.flags) } else { None };
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
                moved(copies, &mut moving, callee, callee.span(inst))
            };
            let new = func.create_inst(shell, &types, span);
            for (old, value) in data.results().zip(func[new].results().collect::<Vec<Value>>()) {
                values.insert(old, value);
            }
            if shared {
                func.insert_before(new, first);
                pool.add(func, new, callee, data.extra);
            } else if fixed {
                func.insert_before(new, first);
            } else {
                func.append_inst(blocks[&from], new);
            }
            if data.opcode == Opcode::Return {
                exits.push(new);
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
        if let Some(&to) = decided.get(&inst) {
            let args: Vec<Value> = callee[to.args].iter().map(|value| values[value]).collect();
            let args = func.push_values(&args);
            let call = BlockCall { block: blocks[&to.block], args, hint: to.hint };
            func[new].extra = Extra::Targets(func.push_block_calls(&[call]));
            continue;
        }
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
                Extra::Mem(mem) if scoping(data.opcode) => {
                    Extra::Mem(func.add_mem(rescoped(callee[mem], past)))
                }
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
    for &exit in &exits {
        for slot in begun.iter().filter_map(|slot| values.get(slot).copied()) {
            let args = func.push_values(&[slot]);
            let data = InstData { args, ..InstData::new(Opcode::LifetimeEnd) };
            let end = func.create_inst(data, &[], func.span(exit));
            func.insert_before(end, exit);
        }
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
    match pool.results.as_mut() {
        Some(results) => {
            results.substitute(func, &forward);
            // The branch the pad's arm was on is out of its block already, and the arm is read
            // again below for the calls of the body.
            if let Some((_, _, pad, _)) = arms {
                func.rewrite(pad.args, |value| crate::uses::chase(&forward, value));
            }
        }
        None => crate::uses::substitute_all(func, &forward),
    }
    func.remove_inst(call);
    func.append_inst(block, jump);
    // What the splice took out is the call and the branch on its unwind, none of which names a
    // clique, so the largest one now is the largest before or one of the accesses just made.
    let added = (mark..func.counts().insts)
        .map(Inst::from_usize)
        .filter(|&inst| func.block_of(inst).is_some());
    pool.highest = Some(past.max(clique_of(func, added)));

    // An unwind out of any of those calls passes through the call that was inlined, so it owes
    // what that call's pad does. Each one gets the edge the lowering gives a call in a handler's
    // scope, an `unwound` and a branch on it to the pad, with the rest of its block moved behind
    // the branch. Several calls sharing one pad is fine, since the code generator finds a call's
    // pad by its branch and no machine edge ever enters one.
    if let Some((_, _, pad, _)) = arms {
        for new in bare {
            let block = func.block_of(new).expect("a copied call is in a block");
            let rest = func.create_block();
            func.move_after(new, rest);
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

/// The blocks of the callee a copy reaches when the call passes `constants` to its entry block, one
/// flag for each block, with the place each `br_if` and `switch` those constants decide goes to.
///
/// A copy is all its callee's blocks otherwise, and the ones a constant argument rules out stayed
/// in the caller until `prune` took them out, sixteen passes later. On zstd_compress.c at `-O2`
/// that was seven copies of the switch in `ZSTD_CCtxParams_setParameter` in
/// `ZSTD_CCtx_setCParams`, each on a constant `param`, and the module was five times the size it is
/// after `prune` for every pass before it. tamnd/rucc#3052. A block is reached from one already
/// reached, so every block that dominates it was walked first and what a constant works out to in
/// it is known by then. A value defined in a block left out is only read in blocks it dominates,
/// which are left out too.
fn reached(
    callee: &Func,
    start: Block,
    constants: &[Option<(Imm, Type)>],
) -> (Vec<bool>, Map<Inst, BlockCall>) {
    let mut values: Map<Value, (Imm, Type)> = callee[start]
        .params
        .iter()
        .zip(constants)
        .filter_map(|(&param, &constant)| Some((param, constant?)))
        .collect();
    let mut live = vec![false; callee.counts().blocks];
    let mut decided = Map::default();
    live[start.index()] = true;
    let mut work = vec![start];
    while let Some(block) = work.pop() {
        for inst in callee.insts(block) {
            let data = &callee[inst];
            let args = &callee[data.args];
            if !values.is_empty() && args.iter().any(|arg| values.contains_key(arg)) {
                // A `__builtin_constant_p` is left for `constant-p` to answer, as it was before.
                if let Some(result) =
                    data.results().next().filter(|_| data.opcode != Opcode::IsConstant)
                {
                    let operand = |arg: Value| {
                        values.get(&arg).copied().or_else(|| crate::fold::constant(callee, arg))
                    };
                    if let Some(found) = computed(callee, inst, &operand) {
                        values.insert(result, found);
                    }
                }
                if let Some(to) = args
                    .first()
                    .filter(|_| callee.is_terminator(inst))
                    .and_then(|arg| values.get(arg))
                    .and_then(|&(value, _)| goes_to(callee, data, value))
                {
                    decided.insert(inst, to);
                    if !live[to.block.index()] {
                        live[to.block.index()] = true;
                        work.push(to.block);
                    }
                    continue;
                }
            }
            for to in callee.successors(inst) {
                if !live[to.block.index()] {
                    live[to.block.index()] = true;
                    work.push(to.block);
                }
            }
        }
    }
    (live, decided)
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

/// Whether `opcode` is one of the instructions `-fsafety-restrict` puts in, whose scope is not a
/// hint that can be dropped but the thing they are about.
///
/// A check with no clique asks nothing and the verifier refuses it, and a `restrict_enter` with
/// none opens no record. The runtime finds a record by walking out to the innermost block with the
/// check's clique, so a body inlined with its own blocks still around it behaves as the call did.
fn scoping(opcode: Opcode) -> bool {
    matches!(
        opcode,
        Opcode::RestrictEnter
            | Opcode::RestrictLeave
            | Opcode::CheckRestrictRead
            | Opcode::CheckRestrictWrite
    )
}

/// One of those, with the callee's clique moved past every clique the caller already has, so that
/// a check of the caller's and one of the inlined body's are never the same check to anything that
/// compares them.
fn rescoped(info: MemInfo, past: u16) -> MemInfo {
    let Restrict { clique, base } = info.restrict;
    if clique == 0 {
        return info;
    }
    let clique = clique.checked_add(past).expect("fewer restrict scopes than that");
    MemInfo { restrict: Restrict { clique, base }, ..info }
}

/// The largest clique any access of `func` names, which is zero when it names none.
fn highest_clique(func: &Func) -> u16 {
    clique_of(func, func.blocks().flat_map(|block| func.insts(block)))
}

/// The largest clique those instructions of `func` name, which is zero when they name none.
fn clique_of(func: &Func, insts: impl Iterator<Item = Inst>) -> u16 {
    let mut highest = 0;
    for inst in insts {
        let mem = match func[inst].extra {
            Extra::Mem(mem) | Extra::Rmw(_, mem) => mem,
            _ => continue,
        };
        highest = highest.max(func[mem].restrict.clique);
    }
    highest
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
///
/// A `static` object goes the same way, which is what lets a function go that only a table names.
/// The kernel's device mapper hands `&dm_dax_ops` to an `alloc_dax` that is an empty stub when DAX
/// is not built, so once the stub is inlined nothing names the table, and the three functions it
/// points at call `dax_get_private`, which nothing defines. Only an object marked droppable goes,
/// and it becomes a declaration rather than leaving the module, as a function does.
pub fn drop_unreferenced(module: &mut Module) {
    write_only(module);
    loop {
        let (calls, elsewhere) = references(module);
        let unread: Vec<GlobalId> = module
            .globals()
            .filter(|&id| {
                let global = &module[id];
                global.droppable && !global.is_declaration() && !elsewhere.contains(&global.name)
            })
            .collect();
        for &id in &unread {
            let global = &mut module[id];
            global.init = None;
            global.linkage = Linkage::External;
            global.droppable = false;
        }
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
        if gone.is_empty() && unread.is_empty() {
            return;
        }
        for id in gone {
            module[id] = declaration(&module[id]);
        }
    }
}

/// Takes out every store to a `static` object that nothing reads, so [`drop_unreferenced`] can
/// take the object as well.
///
/// gcc does this for an object whose address only ever reaches plain stores, and the kernel leans
/// on it for the caches `runtime_const_ptr` reads. fs/file_table.c keeps `__filp_cache` in
/// `.data..ro_after_init`, stores the cache into it once, reads it back only to patch the code
/// that uses it, and from then on every reader takes the address the patch wrote into the code.
/// Once the store reaches that one read nothing reads the object, gcc's object has no
/// `.data..ro_after_init` at all, and rucc's had the section with the object in it.
///
/// Only an object marked droppable is asked about, and only when every use of its address is the
/// address of a store, directly or through a `ptr_add` for a field. A round trip through an
/// integer counts as the same address, because that is how a per-cpu write reaches its `__seg_gs`
/// store. `kprobe_instance` in kernel/kprobes.c is only ever written by `__this_cpu_write`, and
/// gcc folds the cast away and drops the object. Anything done to the integer on the way is a
/// read. A volatile or atomic store keeps the object, and so does the address going anywhere
/// else: into a load, a call, an `asm`, another object's initializer or a block argument.
fn write_only(module: &mut Module) {
    let mut candidates: Set<Symbol> = module
        .globals()
        .map(|id| &module[id])
        .filter(|global| global.droppable && !global.is_declaration())
        .map(|global| global.name)
        .collect();
    if candidates.is_empty() {
        return;
    }
    // Named by another object's initializer or by an alias, which is an address that goes
    // somewhere this cannot follow.
    for id in module.globals() {
        let init = module[id].init.map(|list| &module[list]).unwrap_or_default();
        for datum in init {
            if let Datum::Addr(reloc) | Datum::Away(reloc) | Datum::Apart { to: reloc, .. } = *datum
            {
                candidates.remove(&module[reloc].symbol);
            }
        }
    }
    for id in module.aliases() {
        candidates.remove(&module[id].target);
    }
    // Which values in each function are an address in a candidate, and of which one.
    let mut derived: Vec<Map<Value, Symbol>> = Vec::new();
    for id in module.funcs() {
        let func = &module[id];
        let mut found: Map<Value, Symbol> = Map::default();
        loop {
            let before = found.len();
            for inst in func.blocks().flat_map(|block| func.insts(block)) {
                let data = &func[inst];
                let from = match (data.opcode, data.extra) {
                    (Opcode::GlobalAddr, Extra::Symbol(name)) if candidates.contains(&name) => {
                        Some(name)
                    }
                    (Opcode::PtrAdd | Opcode::PtrToInt | Opcode::IntToPtr, _) => {
                        func[data.args].first().and_then(|base| found.get(base).copied())
                    }
                    _ => None,
                };
                if let (Some(name), Some(result)) = (from, data.results().next()) {
                    found.insert(result, name);
                }
            }
            if found.len() == before {
                break;
            }
        }
        derived.push(found);
    }
    // Any use that is not the address of a plain store, or the base of a `ptr_add`, is a read.
    for (id, found) in module.funcs().zip(&derived) {
        let func = &module[id];
        for inst in func.blocks().flat_map(|block| func.insts(block)) {
            let data = &func[inst];
            match data.extra {
                Extra::Symbol(name) if data.opcode != Opcode::GlobalAddr => {
                    candidates.remove(&name);
                }
                Extra::Call(info) => {
                    if let Some(callee) = func[info].callee {
                        candidates.remove(&callee);
                    }
                }
                _ => {}
            }
            let plain = data.opcode == Opcode::Store
                && !data.flags.contains(Flags::VOLATILE)
                && matches!(data.extra, Extra::Mem(mem) if func[mem].order == MemOrder::NotAtomic);
            for (at, value) in func[data.args].iter().enumerate() {
                let Some(name) = found.get(value) else { continue };
                let step =
                    matches!(data.opcode, Opcode::PtrAdd | Opcode::PtrToInt | Opcode::IntToPtr);
                let fine = (plain && at == 1) || (step && at == 0);
                if !fine {
                    candidates.remove(name);
                }
            }
            for call in func.successors(inst) {
                for value in &func[call.args] {
                    if let Some(name) = found.get(value) {
                        candidates.remove(name);
                    }
                }
            }
        }
    }
    if candidates.is_empty() {
        return;
    }
    let ids: Vec<FuncId> = module.funcs().collect();
    for (id, found) in ids.into_iter().zip(derived) {
        let func = &mut module[id];
        let mut gone = Vec::new();
        for inst in func.blocks().flat_map(|block| func.insts(block)) {
            let data = &func[inst];
            // The stores into the object, and the addresses that only those stores read.
            let into = match data.opcode {
                Opcode::Store => func[data.args].get(1).and_then(|value| found.get(value)),
                _ => data.results().next().and_then(|result| found.get(&result)),
            };
            if into.is_some_and(|name| candidates.contains(name)) {
                gone.push(inst);
            }
        }
        for inst in gone {
            func.remove_inst(inst);
        }
    }
}

/// Marks every `static` object that nothing writes as constant, which is what puts it in `.rodata`.
///
/// gcc does this at `-O1` and up in `ipa_discover_variable_flags`, for an object no other file can
/// see, whose every reference is one the compiler can see, whose address is never taken and which
/// nothing stores to. The kernel has a few of these and gcc's objects keep them with the read only
/// data: `names_0` and `names_512` in lib/errname.c are tables of strings that only a load indexes,
/// and `pt_regs_offset` in arch/x86/kernel/perf_regs.c is the same. rucc kept them in `.data`.
///
/// Only an object marked droppable is asked about, so one the program asked to keep stays where it
/// was, and so does one in a section the program named or one that is thread local. Every use of
/// its address has to be the address of a load, directly or through a `ptr_add` for an element or
/// a field. An atomic load counts as taking the address, since gcc spells one as a call that is
/// handed it. A store of any kind keeps the object written, and so does the address going anywhere
/// else: into a call, an `asm`, a round trip through an integer, another object's initializer, an
/// alias or a block argument.
pub fn read_only(module: &mut Module) {
    let mut candidates: Set<Symbol> = module
        .globals()
        .map(|id| &module[id])
        .filter(|global| {
            global.droppable
                && !global.is_declaration()
                && !global.constant
                && global.section.is_none()
                && global.tls.is_none()
        })
        .map(|global| global.name)
        .collect();
    if candidates.is_empty() {
        return;
    }
    for id in module.globals() {
        let init = module[id].init.map(|list| &module[list]).unwrap_or_default();
        for datum in init {
            if let Datum::Addr(reloc) | Datum::Away(reloc) | Datum::Apart { to: reloc, .. } = *datum
            {
                candidates.remove(&module[reloc].symbol);
            }
        }
    }
    for id in module.aliases() {
        candidates.remove(&module[id].target);
    }
    for id in module.funcs() {
        let func = &module[id];
        let mut found: Map<Value, Symbol> = Map::default();
        loop {
            let before = found.len();
            for inst in func.blocks().flat_map(|block| func.insts(block)) {
                let data = &func[inst];
                let from = match (data.opcode, data.extra) {
                    (Opcode::GlobalAddr, Extra::Symbol(name)) if candidates.contains(&name) => {
                        Some(name)
                    }
                    (Opcode::PtrAdd, _) => {
                        func[data.args].first().and_then(|base| found.get(base).copied())
                    }
                    _ => None,
                };
                if let (Some(name), Some(result)) = (from, data.results().next()) {
                    found.insert(result, name);
                }
            }
            if found.len() == before {
                break;
            }
        }
        for inst in func.blocks().flat_map(|block| func.insts(block)) {
            let data = &func[inst];
            match data.extra {
                Extra::Symbol(name) if data.opcode != Opcode::GlobalAddr => {
                    candidates.remove(&name);
                }
                Extra::Call(info) => {
                    if let Some(callee) = func[info].callee {
                        candidates.remove(&callee);
                    }
                }
                _ => {}
            }
            let load = data.opcode == Opcode::Load
                && matches!(data.extra, Extra::Mem(mem) if func[mem].order == MemOrder::NotAtomic);
            for (at, value) in func[data.args].iter().enumerate() {
                let Some(name) = found.get(value) else { continue };
                let fine = (load || data.opcode == Opcode::PtrAdd) && at == 0;
                if !fine {
                    candidates.remove(name);
                }
            }
            for call in func.successors(inst) {
                for value in &func[call.args] {
                    if let Some(name) = found.get(value) {
                        candidates.remove(name);
                    }
                }
            }
        }
    }
    for id in module.globals().collect::<Vec<GlobalId>>() {
        if candidates.contains(&module[id].name) {
            module[id].constant = true;
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
        let limit = limit.map(|single| Limits { single, ..Limits::o2() });
        let mut names = Interner::new();
        let text = format!("{HEAD}{body}");
        let mut module = rucc_ir::parse(&text, &mut names).expect("the fixture parses");
        let said = format!(
            "{:?}",
            run(
                &mut module,
                &names,
                limit,
                once,
                Isa::baseline(),
                Growth::DEFAULT,
                false,
                Pic::Executable,
                false,
                None,
            )
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
            run(
                &mut module,
                &names,
                None,
                false,
                Isa::baseline(),
                Growth::DEFAULT,
                share,
                Pic::Executable,
                false,
                None,
            );
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

    /// A local that scalar replacement makes a value of keeps a slot of its own, and only the
    /// locals that stay in memory share. Two splices of `put`, whose local goes to a call, share
    /// one slot, and the local of each splice of `get` is its own, so that one `put` does not keep
    /// the value of `get` in memory.
    #[test]
    fn a_local_scalar_replacement_takes_is_not_shared() {
        let body = r#"
func @use(ptr), linkage(external);

func @put(i32), linkage(internal), attrs(always_inline) {
block0(%0: i32):
    %1 = alloca, size 4, align 4
    store %0 -> %1, align 4
    call @use(%1) : (ptr)
    return
}

func @get(i32) -> i32, linkage(internal), attrs(always_inline) {
block0(%0: i32):
    %1 = alloca, size 4, align 4
    store %0 -> %1, align 4
    %2 = load.i32 %1, align 4
    return %2
}

func @g(i32) -> i32, linkage(external) {
block0(%0: i32):
    call @put(%0) : (i32)
    %1 = call @get(%0) : (i32) -> i32
    call @put(%1) : (i32)
    %2 = call @get(%1) : (i32) -> i32
    return %2
}
"#;
        let mut names = Interner::new();
        let text = format!("{HEAD}{body}");
        let mut module = rucc_ir::parse(&text, &mut names).expect("the fixture parses");
        let growth = Growth::DEFAULT;
        run(
            &mut module,
            &names,
            None,
            false,
            Isa::baseline(),
            growth,
            true,
            Pic::Executable,
            false,
            None,
        );
        if let Err(errors) = rucc_ir::verify(&module, &names) {
            panic!("the inliner left invalid IR, {errors:?}");
        }
        let g = module.funcs().find(|&id| names.resolve(module[id].name) == "g").expect("g");
        let func = &module[g];
        let slots: Vec<Value> = func
            .blocks()
            .flat_map(|block| func.insts(block))
            .filter(|&inst| func[inst].opcode == Opcode::Alloca)
            .filter_map(|inst| func[inst].first_result)
            .collect();
        assert_eq!(slots.len(), 3, "{}", rucc_ir::print(&module, &names));
        // The slot that goes to `use` is the same one both times, and no load reads it.
        let given: Set<Value> = func
            .blocks()
            .flat_map(|block| func.insts(block))
            .filter(|&inst| func[inst].opcode == Opcode::Call)
            .flat_map(|inst| func[func[inst].args].to_vec())
            .collect();
        assert_eq!(given.len(), 1, "{}", rucc_ir::print(&module, &names));
        let read: Set<Value> = func
            .blocks()
            .flat_map(|block| func.insts(block))
            .filter(|&inst| func[inst].opcode == Opcode::Load)
            .map(|inst| func[func[inst].args][0])
            .collect();
        assert!(read.is_disjoint(&given), "{}", rucc_ir::print(&module, &names));
        assert_eq!(read.len(), 2, "{}", rucc_ir::print(&module, &names));
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

    /// A pointer to an `always_inline` function handed to an `always_inline` body that calls
    /// through it is a direct call once that body is in, and goes in too, the way `patch_cmp` goes
    /// through `__inline_bsearch` into `poke_int3_handler`. One whose signature is not the call's
    /// stays a call through the pointer.
    #[test]
    fn an_always_inline_function_called_through_a_pointer_an_inlined_body_was_given_goes_in() {
        let body = r#"
func @cmp(i32) -> i32, linkage(internal), attrs(always_inline) {
block0(%0: i32):
    %1 = mul.i32 %0, %0
    return %1
}

func @search(ptr, i32) -> i32, linkage(internal), attrs(always_inline) {
block0(%0: ptr, %1: i32):
    %2 = call_indirect %0(%1) : (i32) -> i32
    return %2
}

func @g(i32) -> i32, linkage(external) {
block0(%0: i32):
    %1 = global_addr @cmp
    %2 = call @search(%1, %0) : (ptr, i32) -> i32
    return %2
}
"#;
        let out = inlined(body);
        let g = &out[out.find("func @g").expect("g is there")..];
        assert!(!g.contains("call"), "{out}");
        assert!(g.contains("mul"), "{out}");
        let other = inlined(&body.replace(
            "    %2 = call_indirect %0(%1) : (i32) -> i32\n    return %2",
            "    %2 = sext.i64 %1\n    %3 = call_indirect %0(%2) : (i64) -> i32\n    return %3",
        ));
        let g = &other[other.find("func @g").expect("g is there")..];
        assert!(g.contains("call_indirect"), "{other}");
    }

    /// A call to a function that never comes back is one gcc predicts is never made, so a body
    /// that is larger than the call stays a call, as `machine_real_restart` does in
    /// `native_machine_emergency_restart`. The same body that comes back goes in.
    #[test]
    fn a_call_to_a_function_that_never_comes_back_is_not_inlined_when_it_grows() {
        let body = r#"
func @warn(i32), linkage(external);

func @stop(i32), linkage(external), attrs(inline_hint, noreturn) {
block0(%0: i32):
    call @warn(%0) : (i32)
    call @warn(%0) : (i32)
    call @warn(%0) : (i32)
    unreachable
}

func @g(i32) {
block0(%0: i32):
    call @stop(%0) : (i32)
    unreachable
}
"#;
        let out = inlined_under(body, Some(40));
        let g = &out[out.find("func @g").expect("g is there")..];
        assert!(g.contains("call @stop"), "{out}");
        let back = inlined_under(&body.replace("inline_hint, noreturn", "inline_hint"), Some(40));
        let g = &back[back.find("func @g").expect("g is there")..];
        assert!(!g.contains("call @stop"), "{back}");
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

    /// A function other files may call, whose body is no larger than a call to it, is inlined and
    /// kept, the way `shmem_is_huge` has to be for the `BUILD_BUG` behind it to fold away. One that
    /// is `weak` stays a call, since another file's body may be the one that runs, and so does one
    /// that is larger than the call.
    #[test]
    fn a_function_no_larger_than_the_call_is_inlined_and_kept() {
        let body = r#"
func @huge(ptr, i64) -> i8, linkage(external) {
block0(%0: ptr, %1: i64):
    %2 = iconst.i8 0
    return %2
}

func @g(ptr) -> i8, linkage(external) {
block0(%0: ptr):
    %1 = iconst.i64 0
    %2 = call @huge(%0, %1) : (ptr, i64) -> i8
    return %2
}
"#;
        let out = inlined_under(body, Some(70));
        assert!(!out.contains("call @huge"), "{out}");
        assert!(out.contains("func @huge(ptr, i64) -> i8, linkage(external) {"), "{out}");
        let weak = inlined_under(&body.replacen("linkage(external)", "linkage(weak)", 1), Some(70));
        assert!(weak.contains("call @huge"), "{weak}");
        let larger = body.replace(
            "    %2 = iconst.i8 0\n    return %2\n}\n\nfunc @g",
            "    %2 = iconst.i8 0\n    %3 = add.i8 %2, %2\n    %4 = add.i8 %3, %3\n    %5 = add.i8 %4, %4\n    return %5\n}\n\nfunc @g",
        );
        let larger = inlined_under(&larger, Some(70));
        assert!(larger.contains("call @huge"), "{larger}");
        assert!(
            inlined_under(body, None).contains("call @huge"),
            "-O0 inlines nothing it need not"
        );
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
        run(
            &mut module,
            &names,
            None,
            true,
            Isa::baseline(),
            Growth::DEFAULT,
            false,
            Pic::Executable,
            false,
            None,
        );
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

    /// With copies told apart, each copy of a body has positions of its own, and a body inlined
    /// into a body that is then inlined twice is two copies, each inside its own copy of the
    /// outer body and each naming the call in it.
    #[test]
    fn each_inlined_copy_has_positions_of_its_own() {
        let text = format!(
            r#"{HEAD}
func @inner(i32) -> i32, linkage(internal), attrs(always_inline) {{
block0(%0: i32):
    %1 = mul.i32 %0, %0
    return %1
}}

func @mid(i32) -> i32, linkage(internal), attrs(always_inline) {{
block0(%0: i32):
    %1 = call @inner(%0) : (i32) -> i32
    %2 = add.i32 %1, %0
    return %2
}}

func @g(i32) -> i32, linkage(external) {{
block0(%0: i32):
    %1 = call @mid(%0) : (i32) -> i32
    %2 = call @mid(%1) : (i32) -> i32
    return %2
}}
"#
        );
        let mut names = Interner::new();
        let mut module = rucc_ir::parse(&text, &mut names).expect("the fixture parses");
        let inner_body = Span::new(10, 20);
        let mid_body = Span::new(30, 50);
        let calls = [Span::new(62, 66), Span::new(70, 74)];
        let g = names.intern("g");
        let mid = names.intern("mid");
        for id in module.funcs().collect::<Vec<FuncId>>() {
            let func = &mut module[id];
            if func.name == g {
                func.declared = Span::new(60, 100);
                respan(func, &[calls[0], calls[1], Span::new(80, 84)]);
            } else if func.name == mid {
                func.declared = mid_body;
                respan(func, &[Span::new(32, 36), Span::new(38, 42), Span::new(44, 48)]);
            } else {
                func.declared = inner_body;
                respan(func, &[Span::new(12, 15), Span::new(16, 18)]);
            }
        }
        module.copies.next = Some(1000);
        run(
            &mut module,
            &names,
            None,
            true,
            Isa::baseline(),
            Growth::DEFAULT,
            false,
            Pic::Executable,
            false,
            None,
        );
        let copies = &module.copies;
        let id = module.funcs().find(|&id| module[id].name == g).expect("g is there");
        let func = &module[id];
        let mut outer = Vec::new();
        for block in func.blocks() {
            for inst in func.insts(block) {
                let span = func.span(inst);
                match func[inst].opcode {
                    Opcode::Mul => {
                        let site = copies.site_at(span.lo).expect("the product is in a copy");
                        let site = copies.sites()[site];
                        assert_eq!(site.of, inner_body);
                        assert_eq!(span, Span::new(site.at + 2, site.at + 5));
                        let up = copies.site_at(site.call.lo).expect("the call is in a copy");
                        let up = copies.sites()[up];
                        assert_eq!(up.of, mid_body);
                        assert_eq!(site.call, Span::new(up.at + 2, up.at + 6));
                        outer.push(up.call);
                    }
                    Opcode::Add => {
                        let site = copies.site_at(span.lo).expect("the sum is in a copy");
                        let site = copies.sites()[site];
                        assert_eq!(site.of, mid_body);
                        assert_eq!(span, Span::new(site.at + 8, site.at + 12));
                    }
                    _ => {}
                }
            }
        }
        outer.sort_by_key(|span| span.lo);
        assert_eq!(outer, calls);
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
        let out = inlined_under(HINTED, Some(0));
        assert!(out.contains("call @bump"), "{out}");
    }

    /// What only works out a constant is not counted against it, since it folds away before gcc
    /// would have measured the body. This is the size switch a `this_cpu_read` comes to, with the
    /// size already a constant, and it is one instruction of work, the load, not ten.
    #[test]
    fn what_folds_away_is_not_counted_against_a_function_declared_inline() {
        let body = r#"
func @node(i32) -> i32, linkage(external), attrs(inline_hint) {
block0(%0: i32):
    %1 = iconst.i64 1
    %2 = iconst.i32 2
    %3 = zext.i64 %2
    %4 = shl %1, %3
    switch %4, block1, [4 => block2]

block1:
    jump block3(%0)

block2:
    %5 = global_addr @numa_node
    %6 = load.i32 %5, align 4
    jump block3(%6)

block3(%7: i32):
    return %7
}

func @g(i32) -> i32, linkage(external) {
block0(%0: i32):
    %1 = call @node(%0) : (i32) -> i32
    return %1
}
"#;
        let out = inlined_under(body, Some(0));
        assert!(out.contains("call @node"), "{out}");
        let out = inlined_under(body, Some(1));
        let g = &out[out.find("func @g").expect("g is there")..];
        assert!(!g.contains("call @node"), "{out}");
    }

    /// A constant the call passes counts as one too, so the same switch over a parameter is one
    /// instruction of work where the call passes the size and three where it does not.
    #[test]
    fn what_a_constant_argument_folds_away_is_not_counted_either() {
        let body = |arg: &str| {
            format!(
                r#"
func @node(i64, i32) -> i32, linkage(external), attrs(inline_hint) {{
block0(%0: i64, %1: i32):
    %2 = shl %0, %0
    switch %2, block1, [4 => block2]

block1:
    jump block3(%1)

block2:
    %3 = global_addr @numa_node
    %4 = load.i32 %3, align 4
    jump block3(%4)

block3(%5: i32):
    return %5
}}

func @g(i64, i32) -> i32, linkage(external) {{
block0(%0: i64, %1: i32):
    %2 = iconst.i64 1
    %3 = call @node({arg}, %1) : (i64, i32) -> i32
    return %3
}}
"#
            )
        };
        let out = inlined_under(&body("%2"), Some(1));
        let g = &out[out.find("func @g").expect("g is there")..];
        assert!(!g.contains("call @node"), "{out}");
        let out = inlined_under(&body("%0"), Some(1));
        assert!(out.contains("call @node"), "{out}");
    }

    /// A copy is only the blocks the constants the call passes leave reachable, so an arm a constant
    /// rules out never gets to the caller and the switch on it is a jump to the arm it picks.
    #[test]
    fn a_copy_leaves_out_what_a_constant_argument_rules_out() {
        let body = |arg: u32| {
            format!(
                r#"
func @pick(i32, i32) -> i32, linkage(internal), attrs(always_inline) {{
block0(%0: i32, %1: i32):
    switch %0, block1, [4 => block2]

block1:
    jump block3(%1)

block2:
    %2 = global_addr @numa_node
    %3 = load.i32 %2, align 4
    jump block3(%3)

block3(%4: i32):
    return %4
}}

func @g(i32) -> i32, linkage(external) {{
block0(%0: i32):
    %1 = iconst.i32 {arg}
    %2 = call @pick(%1, %0) : (i32, i32) -> i32
    return %2
}}
"#
            )
        };
        let g = |out: &str| out[out.find("func @g").expect("g is there")..].to_string();
        let out = inlined(&body(4));
        assert!(!g(&out).contains("call @pick") && !g(&out).contains("switch"), "{out}");
        assert!(g(&out).contains("@numa_node"), "{out}");
        let out = inlined(&body(5));
        assert!(!g(&out).contains("call @pick") && !g(&out).contains("switch"), "{out}");
        assert!(!g(&out).contains("@numa_node"), "{out}");
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

    /// Called from two places, each copy is no larger than the call it replaces, so both go in and
    /// the function goes, as gcc copies it into all its callers.
    #[test]
    fn a_static_function_called_twice_goes_into_both_when_that_is_no_larger() {
        let twice = ONCE.replace(
            "    %1 = call @scale(%0) : (i32) -> i32\n    return %1",
            "    %1 = call @scale(%0) : (i32) -> i32\n    %2 = call @scale(%1) : (i32) -> i32\n    \
             return %2",
        );
        assert_ne!(twice, ONCE);
        let out = inlined_under(&twice, Some(70));
        assert!(!out.contains("call @scale"), "{out}");
        assert!(!out.contains("linkage(internal)"), "{out}");
    }

    /// A `static` function with a cheap arm a flag picks and a longer tail, called from three
    /// places, two of which pass the flag for the cheap arm.
    const PICK: &str = r#"
func @pick(i1, i32) -> i32, linkage(internal) {
block0(%0: i1, %1: i32):
    br_if %0, block1, block2
block1:
    return %1
block2:
    %2 = iconst.i32 3
    %3 = mul.i32 %1, %2
    %4 = iconst.i32 7
    %5 = add.i32 %3, %4
    %6 = iconst.i32 5
    %7 = xor.i32 %5, %6
    %8 = iconst.i32 9
    %9 = mul.i32 %7, %8
    %10 = add.i32 %9, %1
    %11 = xor.i32 %10, %3
    %12 = mul.i32 %11, %5
    %13 = add.i32 %12, %7
    %14 = xor.i32 %13, %9
    %15 = mul.i32 %14, %10
    %16 = add.i32 %15, %11
    %17 = xor.i32 %16, %12
    return %17
}

func @f(i32) -> i32, linkage(external) {
block0(%0: i32):
    %1 = iconst.i1 -1
    %2 = call @pick(%1, %0) : (i1, i32) -> i32
    return %2
}

func @g(i32) -> i32, linkage(external) {
block0(%0: i32):
    %1 = iconst.i1 -1
    %2 = call @pick(%1, %0) : (i1, i32) -> i32
    return %2
}

func @k(i32, i1) -> i32, linkage(external) {
block0(%0: i32, %1: i1):
    %2 = call @pick(%1, %0) : (i1, i32) -> i32
    return %2
}
"#;

    /// The two cheap copies cost less than their calls and the third costs less than the body that
    /// goes, so every call goes in and the function goes, which is what gcc 16 does at `-O1`.
    #[test]
    fn a_static_function_goes_into_all_its_callers_when_that_is_no_larger() {
        let (out, said) = inlined_with(PICK, Some(2), true);
        assert!(!out.contains("call @pick"), "{out}");
        assert!(!out.contains("linkage(internal)"), "{out}");
        assert_eq!(said.matches(ALL_INLINED).count(), 3, "{said}");
        let (out, _) = inlined_with(PICK, None, true);
        assert_eq!(out.matches("call @pick").count(), 3, "{out}");
    }

    /// Without the flag in `f` its copy is the whole body too, and the three copies come to more
    /// than the body, so none of the calls goes in, not even the cheap one in `g`.
    #[test]
    fn a_static_function_whose_copies_grow_the_program_keeps_all_its_calls() {
        let unknown = PICK.replace(
            "@f(i32) -> i32, linkage(external) {\nblock0(%0: i32):\n    %1 = iconst.i1 -1\n",
            "@f(i32, i1) -> i32, linkage(external) {\nblock0(%0: i32, %1: i1):\n",
        );
        assert_ne!(unknown, PICK);
        let out = inlined_under(&unknown, Some(2));
        assert_eq!(out.matches("call @pick").count(), 3, "{out}");
    }

    /// A body that only passes its argument on with five more is eight to gcc, a call and six
    /// arguments and the result, where each call of it is three, so three copies come to more
    /// than the body and gcc 16 keeps the three calls at `-O1`. Counted one to an instruction the
    /// copy was smaller than the call.
    #[test]
    fn a_static_function_that_passes_more_than_it_is_given_stays_a_call() {
        let wrap = r#"
func @sink(i32, i32, i32, i32, i32, i32) -> i32, linkage(external);

func @wrap(i32) -> i32, linkage(internal) {
block0(%0: i32):
    %1 = iconst.i32 1
    %2 = iconst.i32 2
    %3 = iconst.i32 3
    %4 = iconst.i32 4
    %5 = iconst.i32 5
    %6 = call @sink(%0, %1, %2, %3, %4, %5) : (i32, i32, i32, i32, i32, i32) -> i32
    return %6
}

func @f(i32) -> i32, linkage(external) {
block0(%0: i32):
    %1 = call @wrap(%0) : (i32) -> i32
    return %1
}

func @g(i32) -> i32, linkage(external) {
block0(%0: i32):
    %1 = call @wrap(%0) : (i32) -> i32
    return %1
}

func @k(i32) -> i32, linkage(external) {
block0(%0: i32):
    %1 = call @wrap(%0) : (i32) -> i32
    return %1
}
"#;
        let (out, said) = inlined_with(wrap, Some(2), true);
        assert_eq!(out.matches("call @wrap").count(), 3, "{out}");
        assert!(!said.contains(ALL_INLINED), "{said}");
    }

    /// A caller that asked not to be optimized keeps its call, and gcc copies none of the calls
    /// when it cannot copy every one.
    #[test]
    fn a_static_function_one_caller_keeps_stays_a_call_in_every_caller() {
        let kept = PICK.replace(
            "@k(i32, i1) -> i32, linkage(external) {",
            "@k(i32, i1) -> i32, linkage(external), attrs(optnone) {",
        );
        assert_ne!(kept, PICK);
        let out = inlined_under(&kept, Some(2));
        assert_eq!(out.matches("call @pick").count(), 3, "{out}");
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
                // Its address kept somewhere, since a local nothing reads is one gcc has deleted
                // before it measures.
                format!(
                    "    %{number} = alloca, size {size}, align 16\n    store %{number} -> %{number}, align 16\n"
                )
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

    /// A local whose only use is as an argument to another block, which is a pointer that starts at
    /// a buffer and walks it, is in the frame like any other, so a large one stays a call.
    #[test]
    fn a_local_handed_to_another_block_counts_against_the_frame() {
        let fixture = framed(4096, 0, "").replace(
            "    store %5 -> %5, align 16\n    return %4",
            "    jump block1(%5)\n\nblock1(%6: ptr):\n    store %6 -> %6, align 16\n    return %4",
        );
        assert!(fixture.contains("jump block1(%5)"), "{fixture}");
        let (out, said) = inlined_with(&fixture, Some(70), true);
        assert!(out.contains("call @scale"), "{out}");
        assert!(said.contains("stack frame growth limit reached"), "{said}");
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
            run(
                &mut module,
                &names,
                Some(Limits { single: 70, ..Limits::o2() }),
                true,
                Isa::baseline(),
                Growth::CONSERVE,
                false,
                Pic::Executable,
                false,
                None,
            );
            rucc_ir::print(&module, &names)
        };
        assert!(conserved(&framed(256, 0, "")).contains("call @scale"));
        assert!(conserved(&framed(4096, 1024, "")).contains("call @scale"));
        assert!(!conserved(&framed(96, 0, "")).contains("call @scale"));
        assert!(!conserved(&framed(400, 1024, "")).contains("call @scale"));
        // A local only written and compared, the pair `typecheck()` declares, is not in the frame
        // gcc measures, so the same 256 bytes with nothing reading them go in.
        let unread = framed(256, 0, "").replace(
            "    store %5 -> %5, align 16\n",
            "    %6 = icmp eq %5, %5\n    memset %5, %0, size 8, align 16\n",
        );
        assert!(unread.contains("memset %5"), "{unread}");
        assert!(!conserved(&unread).contains("call @scale"));
        // Nor is a local scalar replacement makes values of, which is a `guard()` once its
        // destructor is inlined. The same 128 bytes, written and read back as two words, go in,
        // and they stay a call when the address is kept somewhere as well.
        let scalar = framed(128, 0, "").replace(
            "    store %5 -> %5, align 16
",
            "    %6 = sext.i64 %4\n    store %6 -> %5, align 16\n    %7 = load.i64 %5, align 16\n",
        );
        assert!(scalar.contains("load.i64 %5"), "{scalar}");
        assert!(!conserved(&scalar).contains("call @scale"), "{scalar}");
        let kept = scalar.replace("    %7 = load", "    store %5 -> %5, align 16\n    %7 = load");
        assert!(conserved(&kept).contains("call @scale"), "{kept}");
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

    /// A body with `-fsafety-restrict` in it keeps the scope of its block and of its checks, moved
    /// past the caller's own, while a plain access loses the promise it was given as a hint. Before
    /// this the checks came out in no scope, which the verifier refuses, and the case in
    /// `tests/safety` that passes two aliasing `restrict` pointers did not compile at -O2.
    #[test]
    fn a_body_with_restrict_checks_keeps_their_scope_past_the_callers() {
        let out = inlined(
            r#"
func @combine(ptr), linkage(internal), attrs(always_inline) {
block0(%0: ptr):
    %1 = alloca, size 112, align 8
    restrict_enter %1, size 112, align 8, restrict(1, 1)
    check_restrict_write %0, size 4, align 4, restrict(1, 1)
    %2 = iconst.i32 1
    store %2 -> %0, align 4, restrict(1, 1)
    restrict_leave %1
    return
}

func @main(ptr), linkage(external) {
block0(%0: ptr):
    %1 = alloca, size 112, align 8
    restrict_enter %1, size 112, align 8, restrict(1, 2)
    check_restrict_read %0, size 4, align 4, restrict(1, 2)
    call @combine(%0) : (ptr)
    restrict_leave %1
    return
}
"#,
        );
        let main = &out[out.find("func @main").expect("main")..];
        assert!(!main.contains("call @combine"), "{out}");
        assert_eq!(main.matches("restrict(1, 2)").count(), 2, "{out}");
        assert_eq!(main.matches("restrict(2, 1)").count(), 2, "{out}");
        assert_eq!(main.matches("restrict_leave").count(), 2, "{out}");
    }

    /// A local the safety lowering marked where it begins ends where each copy of its function
    /// returns, since the frame that ended it there is the caller's now and goes on. Without the
    /// marker there is nothing to end, which is every build that is not instrumented.
    #[test]
    fn a_marked_local_ends_where_each_inlined_copy_returns() {
        let body = r#"
func @use(ptr), linkage(external);

func @part(i32) -> i32, linkage(internal), attrs(always_inline) {
block0(%0: i32):
    %1 = alloca, size 16, align 4
    %2 = iconst.i64 16
    meta_begin %1, %2, class automatic
    store %0 -> %1, align 4
    call @use(%1) : (ptr)
    %3 = load.i32 %1, align 4
    %4 = icmp eq %3, %0
    br_if %4, block1, block2

block1:
    return %3

block2:
    return %0
}

func @f(i32) -> i32, linkage(external) {
block0(%0: i32):
    %1 = call @part(%0) : (i32) -> i32
    return %1
}
"#;
        let out = inlined(body);
        let f = &out[out.find("func @f").expect("f is there")..];
        assert!(!f.contains("call @part"), "{out}");
        assert_eq!(f.matches("lifetime_end").count(), 2, "{out}");
        let plain = inlined(&body.replace("    meta_begin %1, %2, class automatic\n", ""));
        let f = &plain[plain.find("func @f").expect("f is there")..];
        assert!(!f.contains("lifetime_end"), "{plain}");
    }
}
