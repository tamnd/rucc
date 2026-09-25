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
//! defines under the name, which for a glibc wrapper is the library function.
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
//! A body that takes the address of one of its own labels is copied with the label, so each copy
//! has an address of its own, which is what gcc does and what `990208-1.c` checks. A body that
//! jumps to such an address, or whose labels a static table holds, is refused, since the copy
//! would still be reaching into the original.

use std::collections::{HashMap, HashSet};

use rucc_base::Symbol;
use rucc_ir::{
    Abi, AsmInfo, AttrSet, Block, BlockCall, BlockCallList, CallInfo, Def, Drains, Extra, Float,
    Func, FuncId, Imm, Inst, InstData, Linkage, MemInfo, MemOrder, Module, Opcode, Restrict,
    Signature, SwitchInfo, Type, VaInfo, Value, ValueList,
};
use rucc_tuple::{Arch, Os};

use crate::Stats;

/// What the step calls itself in a remark, and the name `-fno-inline` turns the declared half off
/// by.
pub const NAME: &str = "inline";

const INLINED: &str = "always_inline call inlined";

const HINT_INLINED: &str = "inline call inlined";

/// Which of the two reasons a function is inlined for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `always_inline`, which is a promise.
    Always,
    /// `inline`, which is a hint taken when the body is small enough.
    Hinted,
}

/// Why a call to an `always_inline` function was not inlined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineFailure {
    /// The function reaches itself through calls of this kind.
    Recursive,
    /// The arguments or the results of the call are not what the body takes and gives.
    Mismatch,
    /// A parameter is a structure passed by value, whose copy the call is what makes.
    ByValue,
    /// The body starts a variable argument list of its own, which only a frame of its own has.
    VaStart,
    /// The body jumps to a label by its address, or a static table holds one of its labels,
    /// either of which would still name the original body from the copy.
    ComputedGoto,
    /// The body calls `setjmp`, whose frame would become the caller's.
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
        }
    }
}

/// Inlines every call to an `always_inline` function that can be, and with a `limit` every call to
/// a function declared `inline` whose body is no larger than that, and says what it did where.
///
/// Then turns every function still holding a `va_arg_pack` into a declaration. See the module
/// documentation for why that is the right thing to do with one.
pub fn run(module: &mut Module, limit: Option<u32>) -> Vec<(FuncId, Stats)> {
    let wanted: HashMap<Symbol, (FuncId, Kind)> = module
        .funcs()
        .filter(|&id| !module[id].is_declaration())
        .filter_map(|id| {
            let set = module[id].attrs.set;
            let kind = if set.contains(AttrSet::ALWAYS_INLINE) {
                Kind::Always
            } else if limit.is_some()
                && set.contains(AttrSet::INLINE_HINT)
                && set.without(AttrSet::NOINLINE | AttrSet::OPTNONE | AttrSet::NAKED) == set
            {
                Kind::Hinted
            } else {
                return None;
            };
            Some((module[id].name, (id, kind)))
        })
        .collect();
    let mut done = Vec::new();
    if !wanted.is_empty() {
        let convention = Convention::of(module);
        let mut state = HashMap::new();
        let limit = limit.map_or(0, |limit| usize::try_from(limit).unwrap_or(usize::MAX));
        let how = How { wanted: &wanted, convention, limit };
        for id in module.funcs().collect::<Vec<FuncId>>() {
            settle(module, id, &how, &mut state, &mut done);
        }
    }
    withdraw(module);
    done
}

/// What stays the same for every function [`settle`] visits.
struct How<'a> {
    /// The functions whose calls are inlined, by name, and why.
    wanted: &'a HashMap<Symbol, (FuncId, Kind)>,
    /// The calling convention the pack is forwarded under.
    convention: Convention,
    /// How many instructions a callee declared `inline` may have.
    limit: usize,
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
    state: &mut HashMap<FuncId, State>,
    done: &mut Vec<(FuncId, Stats)>,
) {
    if state.contains_key(&id) || module[id].is_declaration() {
        return;
    }
    state.insert(id, State::Settling);
    // The calls as the function was written. A call that arrives inside a body being inlined is
    // one the callee's own settling already had its chance at. A function that asked not to be
    // optimized is left with its calls, except for the ones that are a promise.
    let optnone = module[id].attrs.set.contains(AttrSet::OPTNONE);
    let calls: Vec<(Inst, FuncId, Kind)> = {
        let func = &module[id];
        func.blocks()
            .flat_map(|block| func.insts(block))
            .filter_map(|inst| {
                let Extra::Call(info) = func[inst].extra else { return None };
                if func[inst].opcode != Opcode::Call {
                    return None;
                }
                let callee = func[info].callee?;
                let &(callee, kind) = how.wanted.get(&callee)?;
                (kind == Kind::Always || !optnone).then_some((inst, callee, kind))
            })
            .collect()
    };
    let mut stats = Stats::new();
    for (call, callee, kind) in calls {
        let why = |failure: InlineFailure| match kind {
            Kind::Always => failure.why(),
            Kind::Hinted => failure.hint(),
        };
        if callee == id || state.get(&callee) == Some(&State::Settling) {
            stats.missed(why(InlineFailure::Recursive));
            continue;
        }
        settle(module, callee, how, state, done);
        // Measured once the callee is settled, since what is copied is the body with its own
        // calls already inlined.
        if kind == Kind::Hinted && size(&module[callee]) > how.limit {
            stats.missed(why(InlineFailure::TooLarge));
            continue;
        }
        match splice(module, id, call, callee, how.convention, kind) {
            Ok(()) if kind == Kind::Always => stats.optimized(INLINED),
            Ok(()) => stats.optimized(HINT_INLINED),
            Err(failure) => stats.missed(why(failure)),
        }
    }
    state.insert(id, State::Settled);
    if !stats.is_empty() {
        done.push((id, stats));
    }
}

/// How many instructions a body has, which is what the limit on a callee declared `inline` counts.
fn size(func: &Func) -> usize {
    func.blocks().map(|block| func.insts(block).count()).sum()
}

/// Inlines one call, or says why not and leaves the caller as it was.
fn splice(
    module: &mut Module,
    caller: FuncId,
    call: Inst,
    callee: FuncId,
    convention: Convention,
    kind: Kind,
) -> Result<(), InlineFailure> {
    // Out of the module for the length of the splice, so that the callee can be read while the
    // caller is written. The two are different functions, since a call to itself is refused
    // before this.
    let stand_in = Func::new(module[caller].name, Signature::new());
    let mut func = std::mem::replace(&mut module[caller], stand_in);
    let result = check(&func, call, &module[callee], convention, kind)
        .map(|plan| copy(&mut func, call, &module[callee], &plan));
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
    spills: HashMap<Inst, Vec<usize>>,
}

/// Whether one call can be inlined, and what the splice needs to know if it can.
fn check(
    func: &Func,
    call: Inst,
    callee: &Func,
    convention: Convention,
    kind: Kind,
) -> Result<Plan, InlineFailure> {
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
    if callee.signature().params.iter().any(|param| matches!(param.abi, Abi::ByVal { .. })) {
        return Err(InlineFailure::ByValue);
    }

    let fixed = params.len();
    let extras = args[fixed..].to_vec();
    let abis = expand(&func[func[info].varargs], extras.len());
    let groups = func.arg_groups(call).and_then(|groups| past(groups, fixed));
    let mut plan = Plan { fixed, extras, abis, groups, spills: HashMap::new() };
    let outer: Vec<(Type, Abi)> = args[..fixed]
        .iter()
        .enumerate()
        .map(|(at, &arg)| (func[arg].ty, signature.params.get(at).map_or(Abi::Plain, |p| p.abi)))
        .collect();

    if callee.named_blocks().next().is_some() {
        return Err(InlineFailure::ComputedGoto);
    }
    let mut packs = HashSet::new();
    let mut counted = false;
    for block in callee.blocks() {
        for inst in callee.insts(block) {
            match callee[inst].opcode {
                Opcode::VaStart => return Err(InlineFailure::VaStart),
                Opcode::IndirectBr => return Err(InlineFailure::ComputedGoto),
                Opcode::Alloca if kind == Kind::Hinted && !callee[inst].args.is_empty() => {
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
    fn of(module: &Module) -> Self {
        match (module.tuple.arch(), module.tuple.os()) {
            (Arch::X86_64, Os::Windows) => Self::Slots,
            (Arch::X86_64, _) => Self::SysV,
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
fn copy(func: &mut Func, call: Inst, callee: &Func, plan: &Plan) {
    let block = func.block_of(call).expect("a call being inlined is in a block");
    let entry = func.entry().expect("a function with a call in it has a body");

    // The part after the call, which takes the call's results as parameters.
    let after = func.create_block();
    let mut forward = HashMap::new();
    for result in func[call].results().collect::<Vec<Value>>() {
        let ty = func[result].ty;
        forward.insert(result, func.append_param(after, ty));
    }
    let moving: Vec<Inst> = func.insts(block).skip_while(|&inst| inst != call).skip(1).collect();
    for inst in moving {
        func.remove_inst(inst);
        func.append_inst(after, inst);
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
    let passed = func[func[call].args][..plan.fixed].to_vec();
    let mut blocks = HashMap::new();
    let mut values = HashMap::new();
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
    let mut made = Vec::new();
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
            let new = func.create_inst(shell, &types, callee.span(inst));
            for (old, value) in data.results().zip(func[new].results().collect::<Vec<Value>>()) {
                values.insert(old, value);
            }
            if opcode == Opcode::Alloca && data.args.is_empty() {
                let first = func.insts(entry).next().expect("an entry block ends in something");
                func.insert_before(new, first);
            } else {
                func.append_inst(blocks[&from], new);
            }
            made.push((inst, new));
        }
    }

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
    blocks: &HashMap<Block, Block>,
    values: &HashMap<Value, Value>,
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

/// Turns every function that still holds a `va_arg_pack` or a `va_arg_pack_len` into a
/// declaration of the same name.
fn withdraw(module: &mut Module) {
    for id in module.funcs().collect::<Vec<FuncId>>() {
        let func = &module[id];
        let holds = func
            .blocks()
            .flat_map(|block| func.insts(block))
            .any(|inst| matches!(func[inst].opcode, Opcode::VaArgPack | Opcode::VaArgPackLen));
        if !holds {
            continue;
        }
        let mut declared = Func::new(func.name, func.signature().clone());
        declared.spelled = func.spelled;
        declared.visibility = func.visibility;
        declared.attrs = func.attrs;
        declared.declared = func.declared;
        declared.linkage = Linkage::External;
        module[id] = declared;
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;

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
        let mut names = Interner::new();
        let text = format!("{HEAD}{body}");
        let mut module = rucc_ir::parse(&text, &mut names).expect("the fixture parses");
        run(&mut module, limit);
        if let Err(errors) = rucc_ir::verify(&module, &names) {
            panic!("the inliner left invalid IR, {errors:?}\n{}", rucc_ir::print(&module, &names));
        }
        rucc_ir::print(&module, &names)
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
}
