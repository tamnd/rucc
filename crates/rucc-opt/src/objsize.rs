//! The answer to every `object_size` the front end left for the IR.
//!
//! Design: `spec/optimizer/20-idioms-and-libcalls.md` section 20.2, and `spec/13-gnu-compat.md`
//! section 13.5 for the builtin itself.
//!
//! The checker answers `__builtin_object_size` where it can see the object by looking at the
//! expression, and leaves [`Opcode::ObjectSize`] behind where it cannot. What is left is an address
//! read out of a variable, and inside a function that variable is usually one of a handful of
//! addresses a branch or a loop chose between, as in `r = l == 1 ? &a.buf1[5] : &a.buf2[4]`. The
//! IR as the front end writes it has every one of those in front of it: the variable is a block
//! parameter and each branch to its block passes one address, which is an `alloca` or a global with
//! constant offsets on top. This walks that and writes the answer in as a constant.
//!
//! # When it runs
//!
//! Before every other pass and at every level, because nothing after the front end is allowed to
//! see the instruction. The `_chk` folds in [`crate::libcall`] run next and read the answer as the
//! constant they compare a count against, which is the order gcc's own object size pass and its
//! `_chk` folds run in. At `-O0` nothing is walked and every question gets the answer that says
//! nothing, which is what gcc 16.2.0 gives at that level for an address in a variable.
//!
//! # What the walk believes
//!
//! A fixed `alloca` is its size and a dynamic one of a constant count is that count. A global is
//! its size where `extents::vouched` says the definition in the module is the one that
//! will run. What a call returns is the product of the arguments its callee's `alloc_size` names,
//! where each of them is a constant, which is how `p = malloc(10)` has ten bytes behind it. A `ptr_add` of a constant count takes it off what is left, down to nothing past the
//! end, and one going backwards is not followed. A block parameter is every argument every branch
//! to its block passes and a `select` is both of its arms. Anything else is not known.
//!
//! The kinds asking for the largest answer take the largest of a choice and the kinds asking for
//! the smallest take the smallest, and not knowing any part of a choice is not knowing the whole
//! of it. A loop is the one place that needs thought. A parameter reached again while its own
//! answer is being worked out is the pointer carried round the loop unchanged or moved forward,
//! which can only leave less, so for the largest it adds nothing to the choice. Moved backwards it
//! could leave more, and that is why a backwards `ptr_add` is never followed. For the smallest the
//! same holds where the pointer comes round unchanged, and one moved forward each time round may
//! leave as little as anything, so there the smallest is not known.
//!
//! `__builtin_dynamic_object_size` sets a third bit on the kind, and where the walk finds no
//! constant for one of those, an address that is an allocator's result with constant offsets on it
//! is answered with the size the call asked for, multiplied out and less the offset in front of the
//! question, as gcc 16 answers it from `-O1` up. Where the ways into a join bring sizes that are
//! not the same, the join is given a parameter for the size and each way passes its own, which is
//! the PHI gcc builds. See `Walk::running` for which shapes.
//!
//! The dynamic spelling of a kind that asks for the largest answer has one more way to an answer.
//! Where every way to the address starts at the same fixed local or the same global, the answer is
//! the size of that object less how far into it the address is, worked out when the program runs
//! and never below zero. On the way any step is followed, whatever its count and its direction, and
//! so is a library call that gives back an address in the object its first argument is in: a copy
//! or a fill, the `p` spellings that give back the end of what they wrote, the `_chk` form of each,
//! and `strchr (s, 0)`, which gcc reads as `s + strlen (s)`. This is how gcc 16 answers it from
//! `-O1` up, and tcc's `c2str`, which reads each line with `fgets (p, sizeof l - (p - l), fp)` at
//! a `p` a loop moves, gets `__fgets_chk` from both compilers this way. See `Walk::within`.
//!
//! For the closest member the walk stops at the first `ptr_add` lowering marked as the start of a
//! member it knows the length of, and the answer is that length less how far into the member the
//! address is. That is how cxgb3's `memcpy (&d->flit[flits], ...)` is checked against the 128
//! bytes of `flit` whatever `flits` is, as gcc checks it. A member that may run past its end
//! leaves the question not known.
//!
//! The closest member, which is the low bit of the kind, is what a `ptr_add` that lowering wrote
//! for stepping to a member says, as `Extra::Member`. An address the walk finds to be one of
//! those, or one a constant further on, is in that member, which is how `strcpy (inst.buf, s)`
//! asks about the sixteen bytes of `buf` once the fortified `strcpy` is inlined, where gcc asks
//! the same. A member of zero is one that may run past its end, an array at the end of the record a
//! pointer points at or a record that ends in a flexible array, and an address in one is asked
//! about as the whole object, which is what gcc answers for one in a `MEM_REF`. A `trailing` member
//! is an array of a fixed length that may run past its end in the same way. It is as long as it
//! says where the pointer is a constant step into a local or a global, which is what gcc's late
//! pass sees once it has folded the pointer into the address of one, and is the whole object where
//! it is not. Where the walk gets to an object without passing a member, the whole object is an
//! answer no smaller than the member for the largest, so the first kind's answer stands for the
//! second, and for the smallest it could be too big, so there the fourth kind is not known. A step
//! whose count the program worked out, which lowering marks `counted`, is not taken off the
//! member, since gcc's early pass asks before any constant reaches it, and only what is left of
//! the whole object can make the answer smaller. Once every question is answered the members and
//! the marks are taken off, so no pass after this sees one.

use rucc_ir::{
    AllocSize, AttrSet, Counter, Datum, Def, Extra, Flags, Func, FuncId, Imm, Inst, InstData,
    IntPred, MemInfo, MemOrder, Module, Opcode, Pic, Restrict, SymbolRef, Type, Value,
};

use std::cell::{Cell, OnceCell, RefCell};

use rucc_base::hash::{Map, Set};
use rucc_base::{Idx, Interner, Symbol};
use rucc_diag::Span;
use rucc_ir::{Block, BlockCall};

use crate::Cfg;
use crate::alias::{Escapes, Origin, origin};
use crate::dom::Dominators;
use crate::extents::vouched;

/// How far a walk goes before it gives up, which is a chain of block parameters and `ptr_add`
/// this long.
const DEPTH: u32 = 16;

/// How far [`Walk::number`] looks for a number from where it is wanted.
///
/// `kvcalloc (1, PAGE_SIZE, gfp)` asks for a load of `bytes`, which a `__builtin_mul_overflow`
/// stored, of `1 * (1UL << 12)` with the 12 widened from an `int`, and it returns `NULL` on the
/// way the overflow is set. The branch reads that through two `unlikely`s and a block argument,
/// seventeen steps from the constants, and fortify_kunit's `fortify_test_alloc_size_kvmalloc_const`
/// found no size while the size was looked for four steps and the branch sixteen.
const NUMBER: u32 = 24;

/// Answers every `object_size` in the module and says how many there were.
///
/// `look` is false at `-O0`, where every question is answered as not known. A function that
/// `optimize ("O0")` holds to that level is answered the same way whatever `look` is, since gcc
/// reads the level off the function the question is in.
///
/// `library` is the names a call may be read as the standard function under: `None` with
/// `-fno-builtin`, and otherwise every name but the ones `-fno-builtin-<name>` took away.
pub fn answer(
    module: &mut Module,
    names: &Interner,
    library: Option<&[String]>,
    pic: Pic,
    look: bool,
) -> usize {
    let mut answered = 0;
    for id in module.funcs().collect::<Vec<FuncId>>() {
        if module[id].is_declaration() {
            continue;
        }
        let asked = questions(&module[id]);
        if asked.is_empty() {
            continue;
        }
        let look = look && !module[id].attrs.set.contains(AttrSet::OPTNONE);
        let answers: Vec<(Inst, Answer)> = {
            let func = &module[id];
            let cfg = Cfg::new(func);
            let dom = Dominators::new(&cfg);
            let mut walk = Walk {
                module,
                func,
                cfg: &cfg,
                dom: &dom,
                pic,
                names,
                library,
                live: Set::default(),
                numbers: RefCell::default(),
                answered: Map::default(),
                moved: Cell::new(false),
                through: Cell::new(false),
                differed: Cell::new(false),
                ways: Cell::new(WAYS),
                splits: Cell::new(WAYS),
                covered: Cell::new(false),
                places: OnceCell::new(),
                escapes: OnceCell::new(),
                dynamic: Cell::new(false),
            };
            // Twice, since the first round reads every store and every way into a block, and
            // what it rules out can settle a branch the second round reads.
            for _ in 0..2 {
                walk.live = walk.live_edges();
                walk.numbers.borrow_mut().clear();
            }
            let compared = compared(func);
            // Parents before children, so a size a branch in front of a question compares is
            // answered before the question, as in `kmemdup_noprof` where the join that asks
            // about the copy is laid out ahead of the block that asks about the source.
            let mut asked = asked;
            asked.sort_by_key(|&inst| {
                func.block_of(inst).and_then(|block| cfg.rank(block)).unwrap_or(u32::MAX)
            });
            let mut answers = Vec::with_capacity(asked.len());
            for &inst in &asked {
                let answer = walk.ask(inst, look);
                // A constant is what the question will be, so a branch on it is settled from here
                // on, the way it is once the answer is written and folded.
                if let (Answer::Known(number), Some(result)) =
                    (&answer, func[inst].results().next())
                {
                    walk.answered.insert(result, Imm::int(*number, func[result].ty.lane()));
                    if compared.contains(&result) {
                        walk.numbers.borrow_mut().clear();
                        walk.live = walk.live_edges();
                    }
                }
                answers.push((inst, answer));
            }
            answers
        };
        let func = &mut module[id];
        for (inst, answer) in answers {
            match answer {
                Answer::Known(number) => write(func, inst, number),
                answer => put(func, inst, &answer),
            }
            answered += 1;
        }
    }
    for id in module.funcs().collect::<Vec<FuncId>>() {
        forget(&mut module[id]);
    }
    answered
}

/// Takes every `Extra::Member`, `Extra::Array` and `Extra::Trailing` and every `counted` off,
/// which nothing after the questions reads.
fn forget(func: &mut Func) {
    for block in func.blocks().collect::<Vec<_>>() {
        for inst in func.insts(block).collect::<Vec<_>>() {
            if func[inst].extra.marked() {
                func[inst].extra = Extra::None;
            }
            func[inst].flags = func[inst].flags.without(Flags::COUNTED);
        }
    }
}

/// Which of the four questions a walk answers.
#[derive(Clone, Copy)]
struct Ask {
    /// The largest answer rather than the smallest.
    largest: bool,
    /// The closest member rather than the whole object.
    closest: bool,
}

/// The bit above the two of the kind that says the question was asked with
/// `__builtin_dynamic_object_size`, which may be answered with a value worked out at run time.
const DYNAMIC: u8 = 4;

/// The fourth kind, the smallest answer for the closest member, which the walk cannot give.
const SMALLEST_MEMBER: u8 = 3;

/// What one question is answered with.
enum Answer {
    /// A constant, which is every answer the walk finds and every answer that says nothing.
    Known(i128),
    /// The size an allocator was asked for, worked out where the question is.
    Running(Running),
    /// The size of the object the address is in, less how far in the address is, worked out where
    /// the question is.
    Within(Object, u64),
    /// The size an allocator was asked for, less how far into what it gave back the address is,
    /// worked out where the question is, for an address a loop moves.
    Allotted(Value, Running),
    /// What is left of the whole object, worked out where the question is, or what is left of the
    /// member where that is less.
    Capped(Box<Answer>, u64),
    /// A block parameter each way into which brings an answer of its own, worked out in front of
    /// the branch, with every edge into the block and the address it passes. An edge that cannot
    /// be taken has nothing.
    Joined(Block, Vec<Way>),
    /// An answer less the steps added to the address after it, worked out where the question is
    /// and never below zero.
    Less(Box<Answer>, Vec<Value>),
    /// The count of a flexible array member with `counted_by` times the size of an element, read
    /// where the `ptr_add` to the array is, less how far past the start of the array the address
    /// is and never below zero.
    Counted(Value, Counter),
}

/// One edge into a join of [`Answer::Joined`]: the branch, the edge, and the address it passes
/// with the answer for it.
type Way = (Inst, Idx<BlockCall>, Option<(Value, Answer)>);

/// The values of what a `switch` is on that take one way out of it, as [`Walk::chosen`] found
/// them.
enum Chosen {
    /// The cases that go that way.
    Only(Vec<i128>),
    /// Any value but the cases that go elsewhere, where the default goes that way.
    Not(Vec<i128>),
}

impl Chosen {
    /// Whether the value is one of these.
    fn lets(&self, value: i128) -> bool {
        match self {
            Chosen::Only(values) => values.contains(&value),
            Chosen::Not(values) => !values.contains(&value),
        }
    }

    /// Whether a value none of the cases has is one of these.
    fn outside(&self, cases: &[Imm], ty: Type) -> bool {
        match self {
            Chosen::Only(values) => {
                values.iter().any(|&value| cases.iter().all(|case| case.signed(ty) != value))
            }
            Chosen::Not(_) => true,
        }
    }
}

/// The object an address is in, as [`Walk::within`] found it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Object {
    /// A fixed local, which is the `alloca` that made it.
    Local(Value),
    /// A global, by its name.
    Global(Symbol),
    /// The start of a member, which is the `ptr_add` lowering marked, with how long the member is.
    Member(Value, u32),
    /// What an allocator gave back, or a join every way into which brings that.
    Allocated(Value),
}

/// An address that is what an allocator gave back with a constant number of bytes added, as
/// [`Walk::running`] found it.
#[derive(Clone, PartialEq, Eq)]
struct Running {
    /// What the size is made of.
    size: Size,
    /// How far into the object the address is.
    offset: u64,
}

/// The size of the object behind a [`Running`] address.
#[derive(Clone, PartialEq, Eq)]
enum Size {
    /// The arguments of the allocator that multiply to the size, one or two of them.
    Asked(Vec<Value>),
    /// A number the walk found, on one way into a join.
    Fixed(u64),
    /// A block parameter each way into which brings a size of its own, with every edge into the
    /// block and what is left behind the address it passes. An edge that cannot be taken has
    /// nothing.
    Joined(Block, Vec<(Inst, Idx<BlockCall>, Option<Running>)>),
}

/// Every `object_size` in the function.
fn questions(func: &Func) -> Vec<Inst> {
    func.blocks()
        .flat_map(|block| func.insts(block))
        .filter(|&inst| func[inst].opcode == Opcode::ObjectSize)
        .collect()
}

/// Every value a comparison reads.
fn compared(func: &Func) -> Set<Value> {
    func.blocks()
        .flat_map(|block| func.insts(block))
        .filter(|&inst| func[inst].opcode == Opcode::ICmp)
        .flat_map(|inst| func[func[inst].args].iter().copied())
        .collect()
}

/// Puts the constant in place of the question and takes the question away.
fn write(func: &mut Func, inst: Inst, number: i128) {
    let result = func[inst].results().next().expect("an object size is one value");
    let ty = func[result].ty;
    let span = func.span(inst);
    let imm = func.add_imm(Imm::int(number, ty.lane()));
    let data = InstData { extra: Extra::Imm(imm), ..InstData::new(Opcode::IConst) };
    let made = func.create_inst(data, &[ty], span);
    func.insert_before(made, inst);
    let value = func[made].results().next().expect("a constant is one value");
    let forward: Map<_, _> = [(result, value)].into_iter().collect();
    crate::uses::substitute(func, &forward);
    func.remove_inst(inst);
}

/// Puts an answer that is not a constant in place of the question and takes the question away.
fn put(func: &mut Func, inst: Inst, answer: &Answer) {
    let result = func[inst].results().next().expect("an object size is one value");
    let address = func[func[inst].args][0];
    let value = worked(func, inst, address, func[result].ty, answer);
    let forward: Map<_, _> = [(result, value)].into_iter().collect();
    crate::uses::substitute(func, &forward);
    func.remove_inst(inst);
}

/// The answer for an address worked out in front of `inst`, which is not a constant.
///
/// The size an allocator was asked for is less how far in the address is and never below zero,
/// which is what gcc 16 answers `__builtin_dynamic_object_size(p + 2, 0)` with for a `p` that came
/// out of `malloc(n)`. A capped answer is the smaller of the two, which is gcc's `MIN_EXPR`.
fn worked(func: &mut Func, inst: Inst, address: Value, ty: Type, answer: &Answer) -> Value {
    let span = func.span(inst);
    let made = |func: &mut Func, data: InstData, ty: Type| {
        let at = func.create_inst(data, &[ty], span);
        func.insert_before(at, inst);
        func[at].results().next().expect("one result was asked for")
    };
    match answer {
        &Answer::Known(number) => {
            let imm = func.add_imm(Imm::int(number, ty.lane()));
            made(func, InstData { extra: Extra::Imm(imm), ..InstData::new(Opcode::IConst) }, ty)
        }
        Answer::Running(running) => room(func, inst, span, running, ty),
        &Answer::Within(object, size) => {
            let imm = func.add_imm(Imm::int(i128::from(size), ty.lane()));
            let data = InstData { extra: Extra::Imm(imm), ..InstData::new(Opcode::IConst) };
            let size = made(func, data, ty);
            measure(func, inst, address, ty, object, size)
        }
        Answer::Allotted(start, running) => {
            let size = room(func, inst, span, running, ty);
            measure(func, inst, address, ty, Object::Allocated(*start), size)
        }
        Answer::Capped(whole, member) => {
            let whole = worked(func, inst, address, ty, whole);
            let imm = func.add_imm(Imm::int(i128::from(*member), ty.lane()));
            let data = InstData { extra: Extra::Imm(imm), ..InstData::new(Opcode::IConst) };
            let member = made(func, data, ty);
            let args = func.push_values(&[whole, member]);
            let test = InstData {
                args,
                extra: Extra::IntPred(IntPred::Ugt),
                ..InstData::new(Opcode::ICmp)
            };
            let more = made(func, test, ty.with_lane(Type::I1));
            let args = func.push_values(&[more, member, whole]);
            made(func, InstData { args, ..InstData::new(Opcode::Select) }, ty)
        }
        Answer::Joined(block, ways) => {
            let brought: Vec<(Idx<BlockCall>, Value)> = ways
                .iter()
                .map(|(term, at, way)| {
                    let value = match way {
                        Some((address, answer)) => worked(func, *term, *address, ty, answer),
                        None => worked(func, *term, address, ty, &Answer::Known(0)),
                    };
                    (*at, value)
                })
                .collect();
            let size = func.append_param(*block, ty);
            for (at, value) in brought {
                let call = func[at];
                let args = func.append_arg(call.args, value);
                func.set_block_call(at, BlockCall { args, ..call });
            }
            size
        }
        &Answer::Counted(start, counter) => {
            let size = counted(func, start, counter, ty);
            measure(func, inst, address, ty, Object::Member(start, 0), size)
        }
        Answer::Less(whole, steps) => {
            let size = worked(func, inst, address, ty, whole);
            let mut offset = steps[0];
            for &step in &steps[1..] {
                let args = func.push_values(&[offset, step]);
                offset = made(func, InstData { args, ..InstData::new(Opcode::Add) }, ty);
            }
            let imm = func.add_imm(Imm::int(0, ty.lane()));
            let data = InstData { extra: Extra::Imm(imm), ..InstData::new(Opcode::IConst) };
            let zero = made(func, data, ty);
            let args = func.push_values(&[size, offset]);
            let test = InstData {
                args,
                extra: Extra::IntPred(IntPred::Ugt),
                ..InstData::new(Opcode::ICmp)
            };
            let room = made(func, test, ty.with_lane(Type::I1));
            let args = func.push_values(&[size, offset]);
            let left = made(func, InstData { args, ..InstData::new(Opcode::Sub) }, ty);
            let args = func.push_values(&[room, left, zero]);
            made(func, InstData { args, ..InstData::new(Opcode::Select) }, ty)
        }
    }
}

/// How many bytes the count says a flexible array member with `counted_by` has, read right
/// after the `ptr_add` to it, which is where gcc 16 reads it.
///
/// A signed count below zero is no elements, and a signed count of eight bytes is cut to an `int`
/// after that, which is what gcc 16 gives for a `long`. An unsigned one is read as it is.
fn counted(func: &mut Func, start: Value, counter: Counter, ty: Type) -> Value {
    let Def::Result { inst: mut after, .. } = func[start].def else {
        unreachable!("a counted array is a `ptr_add`")
    };
    let span = func.span(after);
    // From the record rather than from the array where the step to the array is a constant, so
    // that a store to the count through the record is the same address and its value is read.
    let (from, delta) = match &func[func[after].args][..] {
        &[base, step] => match func[step].def {
            Def::Result { inst, .. } if func[inst].opcode == Opcode::IConst => {
                let Extra::Imm(imm) = func[inst].extra else { unreachable!("a constant has one") };
                (base, func[imm].signed(func[step].ty) + i128::from(counter.delta))
            }
            _ => (start, i128::from(counter.delta)),
        },
        _ => (start, i128::from(counter.delta)),
    };
    let mut made = |func: &mut Func, data: InstData, ty: Type| {
        let at = func.create_inst(data, &[ty], span);
        func.insert_after(at, after);
        after = at;
        func[at].results().next().expect("one result was asked for")
    };
    let constant = |func: &mut Func, number: i128, ty: Type| {
        let imm = func.add_imm(Imm::int(number, ty.lane()));
        InstData { extra: Extra::Imm(imm), ..InstData::new(Opcode::IConst) }
    };
    let at = if delta == 0 {
        from
    } else {
        let data = constant(func, delta, ty);
        let delta = made(func, data, ty);
        let args = func.push_values(&[from, delta]);
        made(func, InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR)
    };
    let width = Type::int(u32::from(counter.width) * 8);
    let mem = func.add_mem(MemInfo {
        size: u64::from(counter.width),
        align: 1,
        order: MemOrder::NotAtomic,
        tbaa: None,
        owns: 0,
        restrict: Restrict::NONE,
    });
    let args = func.push_values(&[at]);
    let mut count =
        made(func, InstData { args, extra: Extra::Mem(mem), ..InstData::new(Opcode::Load) }, width);
    if counter.signed {
        let data = constant(func, 0, width);
        let zero = made(func, data, width);
        let args = func.push_values(&[count, zero]);
        let test =
            InstData { args, extra: Extra::IntPred(IntPred::Slt), ..InstData::new(Opcode::ICmp) };
        let below = made(func, test, Type::I1);
        let args = func.push_values(&[below, zero, count]);
        count = made(func, InstData { args, ..InstData::new(Opcode::Select) }, width);
        if counter.width == 8 {
            let args = func.push_values(&[count]);
            count = made(func, InstData { args, ..InstData::new(Opcode::Trunc) }, Type::int(32));
        }
    }
    if func[count].ty != ty {
        let widen = if counter.signed { Opcode::SExt } else { Opcode::ZExt };
        let args = func.push_values(&[count]);
        count = made(func, InstData { args, ..InstData::new(widen) }, ty);
    }
    if counter.size == 1 {
        return count;
    }
    let data = constant(func, i128::from(counter.size), ty);
    let size = made(func, data, ty);
    let args = func.push_values(&[count, size]);
    made(func, InstData { args, ..InstData::new(Opcode::Mul) }, ty)
}

/// What is left behind a [`Running`] address, worked out in front of `inst`.
///
/// A join is given a new parameter, and each edge into it passes what is left behind the address
/// it brings, worked out in front of the branch. This is the size gcc 16 builds a PHI for, as in
/// fortify_kunit's `kmemdup ("hello there", len, gfp)`, where the inlined `kmemdup_noprof` gives
/// back `__real_kmemdup (p, 0, gfp)` on the way the check fails and `__real_kmemdup (p, size,
/// gfp)` on the other, and the test wants the answer to be `size`.
fn room(func: &mut Func, inst: Inst, span: Span, running: &Running, ty: Type) -> Value {
    let made = |func: &mut Func, data: InstData, ty: Type| {
        let at = func.create_inst(data, &[ty], span);
        func.insert_before(at, inst);
        func[at].results().next().expect("one result was asked for")
    };
    let number = |func: &mut Func, number: u64| {
        let imm = func.add_imm(Imm::int(i128::from(number), ty.lane()));
        made(func, InstData { extra: Extra::Imm(imm), ..InstData::new(Opcode::IConst) }, ty)
    };
    let mut size = match &running.size {
        Size::Asked(factors) => {
            let mut size = factors[0];
            for &factor in &factors[1..] {
                let args = func.push_values(&[size, factor]);
                size = made(func, InstData { args, ..InstData::new(Opcode::Mul) }, ty);
            }
            size
        }
        &Size::Fixed(left) => number(func, left),
        Size::Joined(block, ways) => {
            let brought: Vec<(Idx<BlockCall>, Value)> = ways
                .iter()
                .map(|(term, at, way)| {
                    let nothing = Running { size: Size::Fixed(0), offset: 0 };
                    (*at, room(func, *term, span, way.as_ref().unwrap_or(&nothing), ty))
                })
                .collect();
            let size = func.append_param(*block, ty);
            for (at, value) in brought {
                let call = func[at];
                let args = func.append_arg(call.args, value);
                func.set_block_call(at, BlockCall { args, ..call });
            }
            size
        }
    };
    if running.offset != 0 {
        let offset = number(func, running.offset);
        let zero = number(func, 0);
        let args = func.push_values(&[size, offset]);
        let test =
            InstData { args, extra: Extra::IntPred(IntPred::Ugt), ..InstData::new(Opcode::ICmp) };
        let some = made(func, test, ty.with_lane(Type::I1));
        let args = func.push_values(&[size, offset]);
        let left = made(func, InstData { args, ..InstData::new(Opcode::Sub) }, ty);
        let args = func.push_values(&[some, left, zero]);
        size = made(func, InstData { args, ..InstData::new(Opcode::Select) }, ty);
    }
    size
}

/// The size of the object less how far into it the address is, worked out in front of the
/// question and never below zero, which is what gcc 16 answers `__builtin_dynamic_object_size (p,
/// 1)` with for a `p` that a loop moves through a local array.
fn measure(
    func: &mut Func,
    inst: Inst,
    address: Value,
    ty: Type,
    object: Object,
    size: Value,
) -> Value {
    let span = func.span(inst);
    let made = |func: &mut Func, data: InstData, ty: Type| {
        let at = func.create_inst(data, &[ty], span);
        func.insert_before(at, inst);
        func[at].results().next().expect("one result was asked for")
    };
    let offset = match steps(func, address, object, ty) {
        // The steps themselves, added up, rather than the difference of two addresses, because
        // the ranges see through an `add` and a `mul` and not through a `ptrtoint`. A branch on
        // `i < 8` in front of `&pr->power.lpi_states[i]` is what proves the room left is at least
        // one element, and gcc folds the fortify check in processor_idle.c on that.
        // The start of the object has all of it in front.
        Some(steps) if steps.is_empty() => return size,
        Some(steps) => {
            let mut offset = steps[0];
            for &step in &steps[1..] {
                let args = func.push_values(&[offset, step]);
                offset = made(func, InstData { args, ..InstData::new(Opcode::Add) }, ty);
            }
            offset
        }
        _ => {
            let start = match object {
                Object::Local(start) | Object::Member(start, _) | Object::Allocated(start) => start,
                // The address of a global is taken again here, since the one the walk passed may
                // be in a block that does not come before the question.
                Object::Global(name) => made(
                    func,
                    InstData { extra: Extra::Symbol(name), ..InstData::new(Opcode::GlobalAddr) },
                    Type::PTR,
                ),
            };
            let args = func.push_values(&[address]);
            let at = made(func, InstData { args, ..InstData::new(Opcode::PtrToInt) }, ty);
            let args = func.push_values(&[start]);
            let from = made(func, InstData { args, ..InstData::new(Opcode::PtrToInt) }, ty);
            let args = func.push_values(&[at, from]);
            made(func, InstData { args, ..InstData::new(Opcode::Sub) }, ty)
        }
    };
    let imm = func.add_imm(Imm::int(0, ty.lane()));
    let zero = made(func, InstData { extra: Extra::Imm(imm), ..InstData::new(Opcode::IConst) }, ty);
    let args = func.push_values(&[size, offset]);
    let test =
        InstData { args, extra: Extra::IntPred(IntPred::Ugt), ..InstData::new(Opcode::ICmp) };
    let room = made(func, test, ty.with_lane(Type::I1));
    let args = func.push_values(&[size, offset]);
    let left = made(func, InstData { args, ..InstData::new(Opcode::Sub) }, ty);
    let args = func.push_values(&[room, left, zero]);
    made(func, InstData { args, ..InstData::new(Opcode::Select) }, ty)
}

/// The most `ptr_add`s [`steps`] follows from an address back to the start of its object.
const STEPS: usize = 8;

/// The steps of the `ptr_add`s that make the address out of the start of the object, nearest the
/// address first, where it is made that way and every step is of the type the answer is.
///
/// Their sum is how far in the address is, the same number as the difference of the two read as
/// integers, since both wrap at the width of a pointer.
fn steps(func: &Func, address: Value, object: Object, ty: Type) -> Option<Vec<Value>> {
    let mut steps = Vec::new();
    let mut value = address;
    loop {
        if matches!(object, Object::Local(start) | Object::Member(start, _) | Object::Allocated(start) if value == start)
        {
            return Some(steps);
        }
        let Def::Result { inst, .. } = func[value].def else { return None };
        let data = &func[inst];
        let started = match (data.opcode, data.extra) {
            (Opcode::GlobalAddr, Extra::Symbol(name)) => object == Object::Global(name),
            _ => false,
        };
        if started {
            return Some(steps);
        }
        let &[base, step] = &func[data.args][..] else { return None };
        if data.opcode != Opcode::PtrAdd || func[step].ty != ty || steps.len() == STEPS {
            return None;
        }
        steps.push(step);
        value = base;
    }
}

/// What a walk over one function reads.
struct Walk<'a> {
    module: &'a Module,
    func: &'a Func,
    cfg: &'a Cfg,
    dom: &'a Dominators,
    pic: Pic,
    names: &'a Interner,
    /// The names a call may be read as the standard function under, as [`answer`] says.
    library: Option<&'a [String]>,
    /// The edges a branch on something already a constant here does not rule out, or empty
    /// while that is still being worked out, which counts every edge.
    live: Set<(Block, Block)>,
    /// What [`Walk::number`] found for each value it was asked about, and how far it was let look
    /// for it, so that a condition many branches share is worked out once. Emptied whenever `live`
    /// changes, since what it found rests on that.
    numbers: RefCell<Map<Value, (Number, u32)>>,
    /// The questions answered so far with a constant, by the value each one was.
    answered: Map<Value, Imm>,
    /// Whether the last walk for the largest answer went round a loop that moves the pointer, so
    /// that what it found is only where the pointer starts.
    moved: Cell<bool>,
    /// Whether the last walk for the closest member stopped at a member of an object it reached
    /// through a pointer, rather than one of a local or a global it stepped into.
    through: Cell<bool>,
    /// Whether the last walk met a join whose ways brought different sizes, which it put
    /// together into the largest or the smallest of them.
    differed: Cell<bool>,
    /// How many more ways into a join [`Walk::running`] may follow for the question it is on, so
    /// that joins one after another, each followed on every way into it, are not followed for a
    /// time that grows as a power of how many there are.
    ways: Cell<u32>,
    /// How many more ways into a join [`Walk::split`] may follow for the question it is on, for
    /// the same reason.
    splits: Cell<u32>,
    /// Whether the join [`Walk::split`] is asked about next is a way into one whose question is
    /// only asked once a branch has found it is not a null pointer, so that the joined pointer is
    /// that one on every way it can bring a null pointer to the question.
    covered: Cell<bool>,
    /// Where each address into a fixed local is used, worked out on the first question that
    /// needs it, as [`Places`] says.
    places: OnceCell<Places>,
    /// Which locals have their address go somewhere, for [`Walk::forwarded`], worked out on the
    /// first load it looks at.
    escapes: OnceCell<Escapes>,
    /// Whether the question is the dynamic spelling, the only one [`Walk::forwarded`] answers.
    /// gcc answers the other in its early pass, before a load has taken the value of a store.
    dynamic: Cell<bool>,
}

/// The addresses a function makes into its fixed locals and every place they are used, found in
/// one pass over the function.
///
/// [`Walk::only_store`] looks at every use of a local for each load from it, and a pass over the
/// whole function for each load is a time that grows as the square of its size. Linux 7.3's
/// lib/tests/fortify_kunit.c inlines many checks into each test, and its `allmodconfig` build
/// spent 157 s of CPU in that pass against 25 s for all of gcc-16. What is in a local and where
/// its addresses go do not change while the function is walked, so they are found once.
#[derive(Default)]
struct Places {
    /// The local each address is in and how far in, for the local itself and for each constant
    /// step forward from it.
    within: Map<Value, (Value, u64)>,
    /// For each local, the instructions that take one of its addresses as an argument, with which
    /// argument it is and how far into the local the address is, in the order of the function.
    uses: Map<Value, Vec<(Inst, usize, u64)>>,
}

/// How many ways into joins [`Walk::running`] follows for one question.
const WAYS: u32 = 64;

/// A number [`Walk::number`] found, with its type, or `None` where it found none.
type Number = Option<(Imm, Type)>;

/// How many bytes are left in front of an address: `Err` where that is not known, `Ok(None)` where
/// the only way to the address is round a loop back to itself, and otherwise the number.
type Left = Result<Option<u64>, ()>;

impl Walk<'_> {
    /// The answer to one question, which is not known at all where `look` is false.
    fn ask(&self, inst: Inst, look: bool) -> Answer {
        let func = self.func;
        let Extra::Question(asked) = func[inst].extra else {
            return Answer::Known(0);
        };
        let Some(result) = func[inst].results().next() else { return Answer::Known(0) };
        let address = func[func[inst].args][0];
        let dynamic = asked & DYNAMIC != 0;
        self.dynamic.set(dynamic);
        self.splits.set(WAYS);
        match look {
            false => Answer::Known(if asked & 2 == 0 { -1 } else { 0 }),
            true => self.reply(address, inst, func[result].ty, asked, DEPTH),
        }
    }

    /// The answer for an address, worked out in front of `at`, to the question `asked` is.
    fn reply(&self, address: Value, at: Inst, ty: Type, asked: u8, depth: u32) -> Answer {
        let (kind, dynamic) = (asked & 3, asked & DYNAMIC != 0);
        let ask = Ask { largest: kind & 2 == 0, closest: kind & 1 == 1 };
        let largest = ask.largest;
        self.moved.set(false);
        self.through.set(false);
        self.differed.set(false);
        self.ways.set(WAYS);
        // The count of a flexible array member with `counted_by` comes before anything else for
        // the dynamic spelling, and for every kind, as gcc 16 reads it off `.ACCESS_WITH_SIZE`.
        // Only for an address made from the array in a straight line: gcc has no answer where a
        // join chose between two of them.
        if dynamic && depth == DEPTH {
            if let Some((start, counter)) = self.counter(address) {
                return Answer::Counted(start, counter);
            }
        }
        let known = self.left(address, ask, DEPTH, &mut Vec::new()).ok().flatten();
        let unknown = if largest { -1 } else { 0 };
        // Ways into a join that bring different sizes are the PHI of them for the dynamic
        // spelling, not the largest or the smallest. mac80211's `ieee80211_parse_tpe_data` copies
        // into `out`, the 5 entries of an EIRP or the 16 of a PSD, and gcc keeps the field spanning
        // WARN that checks the copy against the array it came with.
        if known.is_some() && dynamic && kind != SMALLEST_MEMBER && self.differed.get() {
            let (moved, through) = (self.moved.get(), self.through.get());
            if let Some(joined) = self.split(address, at, ty, asked, depth) {
                return joined;
            }
            self.moved.set(moved);
            self.through.set(through);
        }
        match known {
            // A pointer a loop moves has the most room where it starts, which is what the walk
            // found, and gcc answers the dynamic spelling with the room it has on the trip the
            // question is asked. defxx fills its command with `memcpy (p_addr, ...)` and a
            // `p_addr++` after each entry, and keeps the field spanning WARN that checks it.
            Some(known) if dynamic && self.moved.get() => {
                self.within(address, at, ty, ask).unwrap_or(Answer::Known(i128::from(known)))
            }
            Some(known) if ask.closest && largest && self.through.get() => {
                self.capped(address, at, ty, known, dynamic)
            }
            Some(known) => Answer::Known(i128::from(known)),
            // Only the dynamic spelling may be answered with something worked out while the
            // program runs, and only where the walk found no constant.
            None if dynamic && kind != SMALLEST_MEMBER => self
                .running(address, ask, ty, DEPTH, &mut Vec::new())
                .map(Answer::Running)
                .or_else(|| largest.then(|| self.within(address, at, ty, ask)).flatten())
                .or_else(|| self.split(address, at, ty, asked, depth))
                .unwrap_or(Answer::Known(unknown)),
            None => Answer::Known(unknown),
        }
    }

    /// The `ptr_add` to a flexible array member with `counted_by` that an address is made from by
    /// [`STEPS`] or fewer `ptr_add`s that step to no member, with what it says about the count.
    fn counter(&self, mut value: Value) -> Option<(Value, Counter)> {
        for _ in 0..=STEPS {
            let Def::Result { inst, .. } = self.func[value].def else { return None };
            let data = &self.func[inst];
            if data.opcode != Opcode::PtrAdd {
                return None;
            }
            match data.extra {
                Extra::Counted(counter) => return Some((value, counter)),
                Extra::None => value = *self.func[data.args].first()?,
                _ => return None,
            }
        }
        None
    }

    /// The answer for a block parameter where each way into the block brings one of its own,
    /// which is the PHI of sizes gcc 16 builds for the dynamic spelling. arcmsr's
    /// `arcmsr_sysfs_iop_message_write` copies into `&acb->wqbuffer[acb->put]` or, once it has
    /// wrapped, into `acb->wqbuffer`, and gcc keeps the field spanning WARN that checks the copy
    /// against what is left of the member on the way it came. Every way that can be taken has to
    /// bring something known. A way round a loop is only followed where the address it brings is
    /// worked out afresh on each trip, as [`Walk::afresh`] says, since nothing else could be passed
    /// round it. tls's `tls_split_open_record` copies each entry to `nsge`, which it sets to
    /// `sk_msg_elem (msg_npl, j)` at the end of each trip, and gcc keeps the field spanning WARN
    /// that checks the copy against what is left of the array from entry `j`.
    ///
    /// Steps added to the address after the join are taken off what the ways bring, as tscs454's
    /// `coeff_ram_put` copies to `&coeff_ram[ctl->addr * COEFF_SIZE]` with `coeff_ram` one of
    /// three caches. For the closest member a step into a member is not, since that member is
    /// then the one asked about.
    ///
    /// A way that brings a null pointer to a question only asked once a branch has found the
    /// pointer is not null brings nothing, the way gcc has threaded that branch by the time it
    /// answers. maple_tree's `node_copy` copies to `ma_gaps (dst, d_mt) + d_start` inside `if
    /// (d_gaps)`, and `ma_gaps` gives back one of two members or `NULL`.
    ///
    /// That holds for a way into a join that is itself a way into the one the branch tested too.
    /// dml2's `get_expanded_strategy_list` picks one of four lists by a switch with a default that
    /// leaves it `NULL`, and `insert_strategy_into_expanded_list` copies into an entry of it inside
    /// `if (expanded_strategy_list)`. gcc keeps the field spanning WARN that checks the copy.
    ///
    /// A way a `switch` chose that a later `switch` on the same value turns away from the question
    /// brings nothing either, for the same reason. See [`Walk::onward`].
    fn split(
        &self,
        address: Value,
        question: Inst,
        ty: Type,
        asked: u8,
        depth: u32,
    ) -> Option<Answer> {
        let depth = depth.checked_sub(1)?;
        let mut steps = Vec::new();
        let mut value = address;
        let (block, index) = loop {
            let inst = match self.func[value].def {
                Def::Param { block, index } => break (block, index),
                Def::Result { inst, .. } => inst,
            };
            let data = &self.func[inst];
            let &[base, step] = &self.func[data.args][..] else { return None };
            let member = asked & 1 == 1 && data.extra.marked();
            if data.opcode != Opcode::PtrAdd || member || self.func[step].ty != ty {
                return None;
            }
            if steps.len() == STEPS {
                return None;
            }
            steps.push(step);
            value = base;
        };
        let covered = self.covered.replace(false) && steps.is_empty();
        let unknown = if asked & 2 == 0 { -1 } else { 0 };
        let mut preds = self.cfg.predecessors(block).to_vec();
        preds.sort_unstable();
        preds.dedup();
        let there = self.func.block_of(question)?;
        let guarded = covered || self.guarded(value, question);
        let mut ways = Vec::new();
        for pred in preds {
            let term = self.func.terminator(pred)?;
            let taken = self.taken(pred, block) && self.onward(pred, block, there);
            let round = self.dom.dominates(block, pred);
            for at in self.func.target_list(term).iter() {
                let call = self.func[at];
                if call.block != block {
                    continue;
                }
                if round && !self.afresh(*self.func[call.args].get(index as usize)?, block) {
                    return None;
                }
                let way = match taken {
                    false => None,
                    true => {
                        self.splits.set(self.splits.get().checked_sub(1)?);
                        let arg = *self.func[call.args].get(index as usize)?;
                        let joined = matches!(self.func[arg].def, Def::Param { .. });
                        self.covered.set(guarded && joined);
                        let answer = self.reply(arg, term, ty, asked, depth);
                        self.covered.set(false);
                        match answer {
                            _ if self.null(arg) && guarded => None,
                            Answer::Known(number) if number == unknown => return None,
                            answer => Some((arg, answer)),
                        }
                    }
                };
                ways.push((term, at, way));
            }
        }
        if ways.iter().all(|(_, _, way)| way.is_none()) {
            return None;
        }
        let joined = Answer::Joined(block, ways);
        match steps.is_empty() {
            true => Some(joined),
            false => Some(Answer::Less(Box::new(joined), steps)),
        }
    }

    /// Whether an address is a step from something no join has chosen, so that what is left behind
    /// it can be worked out on a way round a loop from what that trip found. The steps are taken
    /// back to where they start, which has to be a parameter of the function or an instruction
    /// other than a `select`, which may choose the join's own parameter. Either is fine when it is
    /// set before the loop starts, in a block above `header`, and is then the same on every trip.
    /// tls's `tls_get_rec` is inlined into `tls_split_open_record`, so the record `nsge` is in
    /// is the join of what it gave back. A parameter of a block with one way in is what that way
    /// passes, as [`Walk::only`] says.
    fn afresh(&self, mut value: Value, header: Block) -> bool {
        let above = |block: Block| block != header && self.dom.dominates(block, header);
        for _ in 0..DEPTH {
            match self.func[value].def {
                Def::Param { block, index } => {
                    if self.cfg.predecessors(block).is_empty() || above(block) {
                        return true;
                    }
                    match self.only(block, index) {
                        Some(arg) => value = arg,
                        None => return false,
                    }
                }
                Def::Result { inst, .. } if self.func[inst].opcode == Opcode::PtrAdd => {
                    let Some(&base) = self.func[self.func[inst].args].first() else { return false };
                    value = base;
                }
                Def::Result { inst, .. } => {
                    return self.func[inst].opcode != Opcode::Select
                        || self.func.block_of(inst).is_some_and(above);
                }
            }
        }
        false
    }

    /// What a parameter of a block with one way into it is passed on that way, which is the
    /// parameter. An inlined `sk_msg_elem` hands back the address it worked out like that.
    fn only(&self, block: Block, index: u32) -> Option<Value> {
        let &[pred] = self.cfg.predecessors(block) else { return None };
        let term = self.func.terminator(pred)?;
        let mut calls =
            self.func.target_list(term).iter().filter(|&at| self.func[at].block == block);
        let at = calls.next()?;
        if calls.next().is_some() {
            return None;
        }
        self.func[self.func[at].args].get(index as usize).copied()
    }

    /// Whether a way into the join from `pred` can go on to the question, where a `switch` chose
    /// the way and another on the same value comes after the join. gcc has threaded the two by the
    /// time it answers, so a way the second one turns away is not there. mac80211's
    /// `ieee80211_parse_tpe_data` picks `out` by a switch on `interpret` with no default, so `out`
    /// is still null on the default way, and copies to it under a second switch on `interpret`
    /// that only has the cases. The value is the same on both, so the default way never gets to
    /// the copy.
    fn onward(&self, pred: Block, block: Block, question: Block) -> bool {
        let Some((on, chosen)) = self.chosen(pred, block) else { return true };
        let ty = self.func[on].ty;
        let mut seen: Set<Block> = Set::default();
        let mut work = vec![block];
        seen.insert(block);
        while let Some(at) = work.pop() {
            if at == question {
                return true;
            }
            let Some(term) = self.func.terminator(at) else { continue };
            let gate = self.switch_on(term).filter(|&(value, _)| self.same(value, on));
            for (arm, call) in self.func.successors(term).enumerate() {
                let open = match (gate, arm.checked_sub(1)) {
                    (None, _) => true,
                    (Some((_, cases)), Some(case)) => chosen.lets(cases[case].signed(ty)),
                    (Some((_, cases)), None) => chosen.outside(cases, ty),
                };
                if open && self.taken(at, call.block) && seen.insert(call.block) {
                    work.push(call.block);
                }
            }
        }
        false
    }

    /// The value a `switch` chose the way from `pred` into `block` by, and the values of it that
    /// take that way. The `switch` ends `pred`, or a block that `pred` is reached from alone, by
    /// way of blocks that are reached from one block each.
    fn chosen(&self, pred: Block, block: Block) -> Option<(Value, Chosen)> {
        let (mut from, mut to) = (pred, block);
        for _ in 0..STEPS {
            let term = self.func.terminator(from)?;
            if let Some((on, cases)) = self.switch_on(term) {
                let ty = self.func[on].ty;
                let (mut only, mut not, mut default) = (Vec::new(), Vec::new(), false);
                for (arm, call) in self.func.successors(term).enumerate() {
                    match arm.checked_sub(1) {
                        None => default = call.block == to,
                        Some(case) if call.block == to => only.push(cases[case].signed(ty)),
                        Some(case) => not.push(cases[case].signed(ty)),
                    }
                }
                let chosen = if default { Chosen::Not(not) } else { Chosen::Only(only) };
                return Some((on, chosen));
            }
            let mut preds = self.cfg.predecessors(from).to_vec();
            preds.dedup();
            let &[up] = preds.as_slice() else { return None };
            (from, to) = (up, from);
        }
        None
    }

    /// Whether two values are one, or the same conversion of one. Nothing has merged the copies
    /// the front end makes by the time the questions are answered, and mac80211 switches twice on
    /// `interpret`, a `u8` each switch widens on its own.
    fn same(&self, mut a: Value, mut b: Value) -> bool {
        for _ in 0..STEPS {
            if a == b {
                return true;
            }
            let (Def::Result { inst: x, .. }, Def::Result { inst: y, .. }) =
                (self.func[a].def, self.func[b].def)
            else {
                return false;
            };
            let (x, y) = (&self.func[x], &self.func[y]);
            let converts = matches!(x.opcode, Opcode::ZExt | Opcode::SExt | Opcode::Trunc);
            if !converts || x.opcode != y.opcode || self.func[a].ty != self.func[b].ty {
                return false;
            }
            let (Some(&from), Some(&to)) = (self.func[x.args].first(), self.func[y.args].first())
            else {
                return false;
            };
            (a, b) = (from, to);
        }
        false
    }

    /// The value a `switch` is on and its case values, one for each target after the default.
    fn switch_on(&self, term: Inst) -> Option<(Value, &[Imm])> {
        let Extra::Switch(info) = self.func[term].extra else { return None };
        let &on = self.func[self.func[term].args].first()?;
        Some((on, &self.func[self.func[info].cases]))
    }

    /// The closest member of an object reached through a pointer, which gcc answers with what is
    /// left of the member or what is left of the whole object, whichever is less. Its
    /// `addr_object_size` takes that `MIN_EXPR` where the address is in a `MEM_REF` and not where
    /// it is in a declared object. Where the whole object is only known when the program runs, so
    /// is the answer: cxgb4's `t4_alloc_mac_filt` copies each address with `memcpy
    /// (p->macaddr, ...)` at a `p` its loop moves through a command on the stack, and gcc keeps the
    /// field spanning WARN that checks it. The same holds for a structure from `kmalloc (n)`.
    fn capped(&self, address: Value, inst: Inst, ty: Type, member: u64, dynamic: bool) -> Answer {
        let whole = Ask { largest: true, closest: false };
        let known = Answer::Known(i128::from(member));
        self.moved.set(false);
        self.ways.set(WAYS);
        match self.left(address, whole, DEPTH, &mut Vec::new()) {
            Ok(Some(left)) if dynamic && self.moved.get() => {
                match self.within(address, inst, ty, whole) {
                    Some(within) => Answer::Capped(Box::new(within), member),
                    None => Answer::Known(i128::from(member.min(left))),
                }
            }
            Ok(Some(left)) => Answer::Known(i128::from(member.min(left))),
            _ if !dynamic => known,
            _ => {
                self.ways.set(WAYS);
                match self.running(address, whole, ty, DEPTH, &mut Vec::new()) {
                    Some(running) => Answer::Capped(Box::new(Answer::Running(running)), member),
                    None => match self.within(address, inst, ty, whole) {
                        Some(within) => Answer::Capped(Box::new(within), member),
                        None => known,
                    },
                }
            }
        }
    }

    /// Whether an address is a constant or a counted number of steps into a local or a global,
    /// which is a declared object to gcc, rather than into one a pointer points at.
    fn declared(&self, mut value: Value) -> bool {
        for _ in 0..STEPS {
            let Def::Result { inst, .. } = self.func[value].def else { return false };
            let data = &self.func[inst];
            match data.opcode {
                Opcode::Alloca | Opcode::GlobalAddr => return true,
                Opcode::PtrAdd => value = self.func[data.args][0],
                _ => return false,
            }
        }
        false
    }

    /// Whether an address is an element of an array member picked by an index gcc does not know
    /// yet. gcc keeps the array reference there, and an array reference on the way to a member
    /// means the member is not at the end of the structure, even where the array is. mwifiex's
    /// `mwifiex_cfg80211_set_default_key` copies into `key->key_material` with `key` set to
    /// `&priv->wep_key[key_index]`, and gcc keeps the field spanning WARN that checks it.
    fn indexed(&self, value: Value) -> bool {
        let Def::Result { inst, .. } = self.func[value].def else { return false };
        let data = &self.func[inst];
        if data.opcode != Opcode::PtrAdd || data.flags.contains(Flags::COUNTED) {
            return false;
        }
        let args = &self.func[data.args];
        let (Some(&base), Some(&count)) = (args.first(), args.get(1)) else { return false };
        if self.number(count, NUMBER).is_some() {
            return false;
        }
        let Def::Result { inst, .. } = self.func[base].def else { return false };
        let data = &self.func[inst];
        data.opcode == Opcode::PtrAdd
            && matches!(data.extra, Extra::Array(_) | Extra::Trailing(_) | Extra::Counted(_))
    }

    /// How many bytes there are from this address to the end of its object.
    ///
    /// `on` is the block parameters whose answer is being worked out, which is how a loop is seen.
    fn left(&self, value: Value, ask: Ask, depth: u32, on: &mut Vec<Value>) -> Left {
        let largest = ask.largest;
        let depth = depth.checked_sub(1).ok_or(())?;
        match self.func[value].def {
            Def::Param { block, index } => {
                if on.contains(&value) {
                    // Round a loop to where the walk started, which adds nothing to the choice for
                    // the reason the module comment gives. The `ptr_add` below is what decides
                    // whether the smallest can say the same.
                    return Ok(None);
                }
                let preds = self.cfg.predecessors(block);
                if preds.is_empty() {
                    return Err(());
                }
                on.push(value);
                let mut all = Ok(None);
                for &pred in preds {
                    // The `return NULL` of an overflow check on a size that does not overflow,
                    // which `kmalloc_array` has, comes in on an edge that cannot be taken.
                    if !self.taken(pred, block) {
                        continue;
                    }
                    let term = self.func.terminator(pred).ok_or(())?;
                    for call in self.func.successors(term).collect::<Vec<_>>() {
                        if call.block != block {
                            continue;
                        }
                        let arg = *self.func[call.args].get(index as usize).ok_or(())?;
                        let way = self.left(arg, ask, depth, on);
                        if let (Ok(Some(one)), Ok(Some(other))) = (&all, &way) {
                            self.differed.set(self.differed.get() || one != other);
                        }
                        all = both(all, way, largest);
                    }
                }
                on.pop();
                all
            }
            Def::Result { inst, .. } => {
                let data = &self.func[inst];
                let args = &self.func[data.args];
                match data.opcode {
                    Opcode::Select => {
                        let (then, other) = (*args.get(1).ok_or(())?, *args.get(2).ok_or(())?);
                        let then = self.left(then, ask, depth, on);
                        both(then, self.left(other, ask, depth, on), largest)
                    }
                    // The start of a member, which is the closest object to an address in it.
                    Opcode::PtrAdd
                        if ask.closest && data.extra.member().is_some_and(|size| size != 0) =>
                    {
                        let Some(size) = data.extra.member() else { return Err(()) };
                        if !self.declared(*args.first().ok_or(())?) {
                            self.through.set(true);
                        }
                        Ok(Some(u64::from(size)))
                    }
                    // A member that may run past its end is asked about as the whole object,
                    // whatever members the address it is in was worked out from. bcachefs's
                    // `bkey_xattr_init` clears `&k->v` with `k` from `container_of` on `&_k->k`.
                    Opcode::PtrAdd if ask.closest && data.extra.member() == Some(0) => {
                        self.stepped(inst, Ask { closest: false, ..ask }, depth, on)
                    }
                    // An array at the end of the record a pointer points at, which is as long as
                    // it says where the pointer is a step into a declared object and is the whole
                    // object otherwise. mwifiex's `mwifiex_cmd_802_11_key_material_v1` clears the
                    // `key_param_set` array of the WEP layout it casts `&cmd->params.key_material`
                    // to, and gcc answers the closest member there with what `cmd` has left. In
                    // an element of an array member at an unknown index, it is as long as it says.
                    Opcode::PtrAdd if ask.closest && matches!(data.extra, Extra::Trailing(_)) => {
                        let Extra::Trailing(size) = data.extra else { return Err(()) };
                        let base = *args.first().ok_or(())?;
                        // An element at an index gcc does not know is answered as a member is.
                        if self.indexed(base) {
                            if !self.declared(base) {
                                self.through.set(true);
                            }
                            return Ok(Some(u64::from(size)));
                        }
                        let whole = self.stepped(inst, Ask { closest: false, ..ask }, depth, on);
                        match whole {
                            Ok(Some(left)) if self.declared(base) => {
                                Ok(Some(left.min(u64::from(size))))
                            }
                            other => other,
                        }
                    }
                    // An object reached without passing the start of a member, which for the
                    // smallest answer about the closest member may be too big.
                    Opcode::Alloca | Opcode::GlobalAddr | Opcode::Call
                        if ask.closest && !largest =>
                    {
                        Err(())
                    }
                    Opcode::PtrAdd => self.stepped(inst, ask, depth, on),
                    Opcode::Alloca => match args.first() {
                        None => {
                            let Extra::Mem(mem) = data.extra else { return Err(()) };
                            Ok(Some(self.func[mem].size))
                        }
                        Some(&count) => {
                            let (imm, _) = self.number(count, NUMBER).ok_or(())?;
                            Ok(Some(u64::try_from(imm.unsigned()).map_err(|_| ())?))
                        }
                    },
                    Opcode::GlobalAddr => {
                        let Extra::Symbol(name) = data.extra else { return Err(()) };
                        self.global(name).ok_or(())
                    }
                    // An address read back out of a local it was put in once, which is what a
                    // `__free (kfree)` variable is: the cleanup takes its address, so it stays a
                    // slot rather than becoming a value, and the question is asked of the load.
                    Opcode::Load => {
                        if let Some(name) = self.pointed(inst) {
                            if ask.closest && !largest {
                                return Err(());
                            }
                            return self.global(name).ok_or(());
                        }
                        let put = self.stored(*args.first().ok_or(())?, inst).ok_or(())?;
                        self.left(put, ask, depth, on)
                    }
                    // What an allocator gave back, where the attribute on it says which arguments
                    // are the size and each of them is a constant here.
                    Opcode::Call => {
                        let (alloc, args) = self.allocation(inst).ok_or(())?;
                        let mut size: u64 = 1;
                        for factor in factors(alloc, args).ok_or(())? {
                            let (imm, ty) = self.number(factor, NUMBER).ok_or(())?;
                            let factor = u64::try_from(imm.signed(ty)).map_err(|_| ())?;
                            size = size.checked_mul(factor).ok_or(())?;
                        }
                        Ok(Some(size))
                    }
                    _ => Err(()),
                }
            }
        }
    }

    /// How many bytes are left in front of a `ptr_add` of a constant, which is what its base has
    /// left less the step.
    fn stepped(&self, inst: Inst, ask: Ask, depth: u32, on: &mut Vec<Value>) -> Left {
        let largest = ask.largest;
        let data = &self.func[inst];
        let args = &self.func[data.args];
        let value = data.results().next().ok_or(())?;
        let base = *args.first().ok_or(())?;
        let count = *args.get(1).ok_or(())?;
        let (imm, ty) = self.number(count, NUMBER).ok_or(())?;
        let step = u64::try_from(imm.signed(ty)).map_err(|_| ())?;
        // A count the program worked out, even one that comes to a constant, is not known when
        // gcc's early pass asks about the member, so it answers with all of the member and the
        // step is taken off only what is left of the whole object. metronomefb clears `args + i`
        // with `i` one and a size that runs into the `csum` after it, and gcc says nothing.
        if ask.closest && largest && data.flags.contains(Flags::COUNTED) {
            let Some(member) = self.left(base, ask, depth, on)? else {
                self.moved.set(self.moved.get() || step != 0);
                return Ok(None);
            };
            let whole = Ask { closest: false, ..ask };
            return Ok(Some(match self.left(value, whole, depth, on) {
                Ok(Some(whole)) => member.min(whole),
                _ => member,
            }));
        }
        match self.left(base, ask, depth, on)? {
            Some(left) => Ok(Some(left.saturating_sub(step))),
            // A pointer moved forward each time round a loop may end up anywhere further along,
            // so there is no smallest.
            None if !largest && step != 0 => Err(()),
            None => {
                self.moved.set(self.moved.get() || step != 0);
                Ok(None)
            }
        }
    }

    /// A number this value is whatever way the function runs, worked out the way the fold pass
    /// would once it got there.
    ///
    /// This runs before any of the passes, so it looks further than [`crate::fold::evaluated`]
    /// does: through a local stored once and read back, and through an overflow checking
    /// operation on two numbers. `kmalloc_array (256, 3, ...)` is both, a size put into `bytes`
    /// by `__builtin_mul_overflow` and read back for the call to the allocator.
    fn number(&self, value: Value, depth: u32) -> Option<(Imm, Type)> {
        if let Some(found) = crate::fold::constant(self.func, value) {
            return Some(found);
        }
        // Nothing found with less room to look is asked again with more.
        if let Some(&(found, room)) = self.numbers.borrow().get(&value) {
            if found.is_some() || room >= depth {
                return found;
            }
        }
        let found = self.worked_out(value, depth);
        self.numbers.borrow_mut().insert(value, (found, depth));
        found
    }

    /// [`Walk::number`] for a value that is not a constant itself and was not asked about yet.
    fn worked_out(&self, value: Value, depth: u32) -> Option<(Imm, Type)> {
        let next = depth.checked_sub(1)?;
        let (inst, index) = match self.func[value].def {
            Def::Result { inst, index } => (inst, index),
            // The same number on every way in that can be taken, which is how an inlined
            // `mem_alloc_profiling_enabled ()` hands back its `false`.
            Def::Param { block, index } => {
                let mut found = None;
                for &pred in self.cfg.predecessors(block) {
                    if !self.taken(pred, block) {
                        continue;
                    }
                    let term = self.func.terminator(pred)?;
                    for call in self.func.successors(term).collect::<Vec<_>>() {
                        if call.block != block {
                            continue;
                        }
                        let arg = *self.func[call.args].get(index as usize)?;
                        let (imm, ty) = self.number(arg, next)?;
                        if found.is_some_and(|(had, _): (Imm, Type)| had != imm) {
                            return None;
                        }
                        found = Some((imm, ty));
                    }
                }
                return found;
            }
        };
        let data = &self.func[inst];
        let args = &self.func[data.args];
        let ty = self.func[value].ty;
        if !ty.is_int() || !ty.is_scalar() {
            return None;
        }
        match data.opcode {
            Opcode::ObjectSize => self.answered.get(&value).map(|&imm| (imm, ty)),
            Opcode::Load => self.number(self.only_store(*args.first()?, inst)?, next),
            Opcode::Expect => self.number(*args.first()?, next),
            _ if data.results == 2 => {
                let a = self.number(*args.first()?, next);
                let b = self.number(*args.get(1)?, next);
                let at = self.func[data.results().next()?].ty;
                if let (Some((a, _)), Some((b, _))) = (a, b) {
                    let (sum, over) = crate::fold::overflowing(data.opcode, a, b, at)?;
                    let found = if index == 0 { sum } else { Imm::from_bits(u128::from(over)) };
                    return Some((found, ty));
                }
                // A product with 0 or 1 in it fits whatever the other side is, as long as both
                // sides are as wide as the result. `kcalloc (1, size, gfp)` checks `1 * size` in
                // the `kmalloc_array` it calls, and gcc has folded that check away by the time it
                // answers about what came back, so the way out with a null pointer is gone.
                let small =
                    |found: Option<(Imm, Type)>| found.is_some_and(|(imm, _)| imm.unsigned() <= 1);
                let product = matches!(data.opcode, Opcode::UMulOverflow | Opcode::SMulOverflow);
                let wide = args.iter().all(|&arg| self.func[arg].ty == at);
                (index == 1 && product && wide && (small(a) || small(b)))
                    .then(|| (Imm::from_bits(0), ty))
            }
            _ if data.results == 1 => {
                let found = crate::fold::arithmetic(data, args, ty, &|arg| self.number(arg, next))
                    .or_else(|| self.extreme(data, args, next))?;
                Some((found, ty))
            }
            _ => None,
        }
    }

    /// What an unsigned comparison gives when one side is a number as small or as large as its
    /// type goes, which settles it whatever the other side is.
    ///
    /// `kmemdup_noprof` in 6.12's `<linux/fortify-string.h>` returns one allocation on the way
    /// `p_size < size` is true and another on the way it is false, and `p_size` is the size of
    /// the source, which is `(size_t)-1` where it is not known. gcc folds the test before it
    /// answers the question about what came back, and fortify_kunit's `fortify_test_realloc_size`
    /// failed while that took both ways.
    fn extreme(&self, data: &InstData, args: &[Value], depth: u32) -> Option<Imm> {
        let Extra::IntPred(pred) = data.extra else { return None };
        let &[a, b] = args else { return None };
        if data.opcode != Opcode::ICmp {
            return None;
        }
        let top = |(imm, ty): (Imm, Type)| imm == Imm::int(-1, ty);
        let bottom = |(imm, _): (Imm, Type)| imm.unsigned() == 0;
        let (a, b) = (self.number(a, depth), self.number(b, depth));
        let holds = match pred {
            IntPred::Ult if a.is_some_and(top) || b.is_some_and(bottom) => false,
            IntPred::Ugt if a.is_some_and(bottom) || b.is_some_and(top) => false,
            IntPred::Uge if a.is_some_and(top) || b.is_some_and(bottom) => true,
            IntPred::Ule if a.is_some_and(bottom) || b.is_some_and(top) => true,
            _ => return None,
        };
        Some(Imm::from_bits(u128::from(holds)))
    }

    /// Whether the edge can be taken, as far as is known so far.
    fn taken(&self, from: Block, to: Block) -> bool {
        self.live.is_empty() || self.live.contains(&(from, to))
    }

    /// Whether some edge that can be taken comes into the block, or it is the entry.
    fn reached(&self, block: Block) -> bool {
        self.func.entry() == Some(block)
            || self.cfg.predecessors(block).iter().any(|&pred| self.taken(pred, block))
    }

    /// The edges out of each block the entry reaches, leaving out the one a branch on a number
    /// does not take.
    fn live_edges(&self) -> Set<(Block, Block)> {
        let mut live = Set::default();
        let Some(entry) = self.func.entry() else { return live };
        let mut seen: Set<Block> = Set::default();
        let mut work = vec![entry];
        seen.insert(entry);
        while let Some(block) = work.pop() {
            let Some(term) = self.func.terminator(block) else { continue };
            let calls: Vec<Block> = self.func.successors(term).map(|call| call.block).collect();
            let taken = match (self.func[term].opcode, calls.as_slice()) {
                (Opcode::BrIf, &[then, other]) => {
                    let cond = self.func[self.func[term].args].first().copied();
                    match cond.and_then(|cond| self.number(cond, NUMBER)) {
                        Some((imm, _)) if imm.unsigned() != 0 => vec![then],
                        Some(_) => vec![other],
                        None => calls,
                    }
                }
                _ => calls,
            };
            for to in taken {
                live.insert((block, to));
                if seen.insert(to) {
                    work.push(to);
                }
            }
        }
        live
    }

    /// The size of a global, where `extents::vouched` says the definition in the module is the
    /// one that runs.
    fn global(&self, name: Symbol) -> Option<Option<u64>> {
        let Some(SymbolRef::Global(id)) = self.module.lookup(name) else { return None };
        let global = &self.module[id];
        vouched(global, self.pic).then_some(Some(global.size))
    }

    /// The global a load reads the address of, when it loads straight out of a read only global
    /// whose image is that address with nothing added.
    ///
    /// The questions are answered before the loads of read only globals are folded, so
    /// `__compiletime_strlen(ptr_of_11)` in fortify_kunit's `fortify_test_known_sizes` asks about
    /// a load of `static const char *ptr_of_11 = "this is 11!"`. gcc answers 12 there, and the
    /// test failed on the 6.12 arm64 nightly while it was not known.
    fn pointed(&self, load: Inst) -> Option<Symbol> {
        let &addr = self.func[self.func[load].args].first()?;
        let Def::Result { inst, .. } = self.func[addr].def else { return None };
        let data = self.func[inst];
        if data.opcode != Opcode::GlobalAddr {
            return None;
        }
        let Extra::Symbol(name) = data.extra else { return None };
        let Some(SymbolRef::Global(id)) = self.module.lookup(name) else { return None };
        let global = &self.module[id];
        if !global.constant || !vouched(global, self.pic) {
            return None;
        }
        let read = self.func[load].first_result?;
        let pointer = self.module.datalayout.pointer_bits / 8;
        if !self.func[read].ty.is_ptr() {
            return None;
        }
        match self.module[global.init?] {
            [Datum::Addr(reloc), ..] => {
                let reloc = &self.module[reloc];
                (reloc.addend == 0 && reloc.size == pointer).then_some(reloc.symbol)
            }
            _ => None,
        }
    }

    /// The value the store that reaches this load put in a fixed local, when that is all the load
    /// can read.
    ///
    /// The load may read the local itself or a part of it a constant step in, such as the `cmd`
    /// of iommufd's `struct iommufd_ucmd`, which is set to the address of a buffer on the stack
    /// and handed to `copy_struct_from_user`. One store of the bytes the load reads, of the type
    /// it reads, comes before the load on every way to it, and no other store that writes any of
    /// those bytes can get to the load without passing through that one again. The load then
    /// reads what that store put there the last time it ran, and a walk of the stored value
    /// answers for every time it ran.
    ///
    /// Other stores are allowed because the inliner gives two inlined bodies one slot when their
    /// lifetimes do not overlap. keyboard.c's `vt_do_diacrit` has two `__free (kfree)` buffers in
    /// one slot that way, one in each of two cases of its switch.
    ///
    /// Where the address of the local goes anywhere else, to a call or into memory, whatever got
    /// it may write the local from then on, so the load must not be reachable from there at all.
    /// iommufd hands `&ucmd` to the handler only after the copy.
    fn only_store(&self, slot: Value, load: Inst) -> Option<Value> {
        let (local, at) = self.slot(slot)?;
        let read = self.func[load].first_result?;
        let width = self.bytes(self.func[read].ty);
        let places = self.places.get_or_init(|| self.places());
        let mut stores = Vec::new();
        let mut away = Vec::new();
        for &(inst, index, from) in places.uses.get(&local).map_or(&[][..], Vec::as_slice) {
            let data = &self.func[inst];
            let args = &self.func[data.args];
            match (data.opcode, index) {
                // A read, a question or a comparison of the address hands it to nobody, and
                // the slot shares its address with other inlined locals that a guard's
                // cleanup compares.
                (Opcode::Load | Opcode::ObjectSize, 0)
                | (Opcode::Memcpy | Opcode::Memmove, 1)
                | (Opcode::LifetimeEnd | Opcode::ICmp, _) => {}
                // A step to another part of the local, whose uses are looked at in turn.
                (Opcode::PtrAdd, 0)
                    if data.first_result.is_some_and(|step| {
                        places.within.get(&step).is_some_and(|&(other, _)| other == local)
                    }) => {}
                // Something on a way through the function that cannot be taken never runs.
                _ if !self.reached(self.func.block_of(inst)?) => {}
                (Opcode::Store, 1) => {
                    let wrote = self.bytes(self.func[args[0]].ty);
                    if from < at.saturating_add(width) && at < from.saturating_add(wrote) {
                        stores.push((inst, from));
                    }
                }
                // `-ftrivial-auto-var-init=zero` clears a local before the program writes
                // it, which the kernel builds with. The clearing is a store as well, one this
                // walk cannot read, and it is fine for as long as a later one is the one the
                // load sees.
                (Opcode::Memset, 0) => stores.push((inst, from)),
                _ => away.push(inst),
            }
        }
        // The stores before the load on every way to it are one after another, so the last of
        // them is the one each of those ways went through most recently.
        let mut store: Option<(Inst, u64)> = None;
        for &(each, from) in &stores {
            if self.precedes(each, load) && store.is_none_or(|(had, _)| self.precedes(had, each)) {
                store = Some((each, from));
            }
        }
        let (store, from) = store?;
        if stores.iter().any(|&(other, _)| other != store && self.reaches(other, load, store)) {
            return None;
        }
        if self.func[store].opcode != Opcode::Store || from != at {
            return None;
        }
        if away.iter().any(|&went| self.reaches(went, load, went)) {
            return None;
        }
        let put = self.func[self.func[store].args][0];
        (self.func[put].ty == self.func[read].ty).then_some(put)
    }

    /// How many bytes a value of the type takes in memory, an address what the target's take. A
    /// type with no width in the IR is taken as 16 bytes, which can only make a store next to it
    /// look as if it wrote it. f2fs's `make_dentry_ptr_block` sets `bitmap` and then `dentry`
    /// right after it, which as 16 bytes would have run over the first.
    fn bytes(&self, ty: Type) -> u64 {
        let bits = u64::from(ty.bits()) * u64::from(ty.lanes());
        match ty.is_ptr() {
            true => u64::from(self.module.datalayout.pointer_bits / 8),
            false if bits == 0 => 16,
            false => bits.div_ceil(8),
        }
    }

    /// What a load of an address reads, where a store this walk can see is all it can read: the
    /// one store to a local of [`Walk::only_store`], or the store just above it of
    /// [`Walk::forwarded`].
    fn stored(&self, address: Value, load: Inst) -> Option<Value> {
        self.only_store(address, load)
            .or_else(|| self.dynamic.get().then(|| self.forwarded(load)).flatten())
    }

    /// The value the store just above a load put at its address, where the address is in what a
    /// pointer points at rather than in a local, and nothing between the two can write it.
    ///
    /// ufs's `ufs_symlink` sets `inode->i_link` to the symlink kept in the inode and then copies
    /// the name to `inode->i_link`. gcc's loads have taken the value of the store in front of
    /// them by the time it asks, so it measures the copy against the 60 bytes of `i_symlink` and
    /// keeps the field spanning WARN. The walk goes up the block and on into the one way into it
    /// that can be taken, for as long as there is only one. It passes what writes no memory, what
    /// writes a local whose address never leaves the function, and a store a constant step away
    /// from the load in the same object. Anything else that may write memory stops it, a call or
    /// a `volatile` store included.
    fn forwarded(&self, load: Inst) -> Option<Value> {
        let address = *self.func[self.func[load].args].first()?;
        let read = self.func[load].first_result?;
        let (base, Some(at)) = origin(self.func, address) else { return None };
        if !matches!(base, Origin::Unknown(_)) || self.func[load].flags.contains(Flags::VOLATILE) {
            return None;
        }
        let escapes = self.escapes.get_or_init(|| Escapes::of(self.func));
        let width = i64::try_from(self.bytes(self.func[read].ty)).ok()?;
        let mut block = self.func.block_of(load)?;
        let mut until = Some(load);
        for _ in 0..DEPTH {
            let insts: Vec<Inst> = self.func.insts(block).collect();
            let end = until
                .and_then(|it| insts.iter().position(|&inst| inst == it))
                .unwrap_or(insts.len());
            for &inst in insts[..end].iter().rev() {
                let data = &self.func[inst];
                if !data.opcode.writes_memory() {
                    continue;
                }
                let args = &self.func[data.args];
                let to = match data.opcode {
                    Opcode::Store => args[1],
                    Opcode::Memset | Opcode::Memcpy | Opcode::Memmove | Opcode::LifetimeEnd => {
                        *args.first()?
                    }
                    _ => return None,
                };
                match origin(self.func, to) {
                    (Origin::Local(local), _) if !escapes.escaped(local) => {}
                    (other, Some(from))
                        if other == base
                            && data.opcode == Opcode::Store
                            && !data.flags.contains(Flags::VOLATILE) =>
                    {
                        let put = args[0];
                        if from == at {
                            return (self.func[put].ty == self.func[read].ty).then_some(put);
                        }
                        let wrote = i64::try_from(self.bytes(self.func[put].ty)).ok()?;
                        if from < at.saturating_add(width) && at < from.saturating_add(wrote) {
                            return None;
                        }
                    }
                    _ => return None,
                }
            }
            // The way back round a `do { } while (0)` is never taken, and only the ways that
            // can be count.
            let mut preds =
                self.cfg.predecessors(block).iter().filter(|&&pred| self.taken(pred, block));
            let (Some(&pred), None) = (preds.next(), preds.next()) else { return None };
            block = pred;
            until = None;
        }
        None
    }

    /// The fixed local an address is in, with how far in, where the address is the local or a
    /// constant step from it.
    fn slot(&self, address: Value) -> Option<(Value, u64)> {
        let mut at = 0u64;
        let mut value = address;
        loop {
            let Def::Result { inst, .. } = self.func[value].def else { return None };
            let data = &self.func[inst];
            let args = &self.func[data.args];
            match data.opcode {
                Opcode::Alloca if args.is_empty() => return Some((value, at)),
                Opcode::PtrAdd => {
                    let (imm, ty) = crate::fold::constant(self.func, *args.get(1)?)?;
                    at = at.checked_add(u64::try_from(imm.signed(ty)).ok()?)?;
                    value = args[0];
                }
                _ => return None,
            }
        }
    }

    /// Each address the function makes by constant steps forward from the start of a local, with
    /// the local and how far in it is, and where each of them is used.
    fn places(&self) -> Places {
        let mut places = Places::default();
        let insts: Vec<Inst> =
            self.func.blocks().flat_map(|block| self.func.insts(block)).collect();
        let mut grew = true;
        while grew {
            grew = false;
            for &inst in &insts {
                let data = &self.func[inst];
                let Some(value) = data.first_result else { continue };
                if places.within.contains_key(&value) {
                    continue;
                }
                let args = &self.func[data.args];
                let place = match data.opcode {
                    Opcode::Alloca if args.is_empty() => (value, 0),
                    Opcode::PtrAdd => {
                        let Some(&(local, from)) =
                            args.first().and_then(|base| places.within.get(base))
                        else {
                            continue;
                        };
                        let Some((imm, ty)) = crate::fold::constant(self.func, args[1]) else {
                            continue;
                        };
                        let Some(at) =
                            u64::try_from(imm.signed(ty)).ok().and_then(|n| from.checked_add(n))
                        else {
                            continue;
                        };
                        (local, at)
                    }
                    _ => continue,
                };
                places.within.insert(value, place);
                grew = true;
            }
        }
        for &inst in &insts {
            for (index, arg) in self.func[self.func[inst].args].iter().enumerate() {
                if let Some(&(local, from)) = places.within.get(arg) {
                    places.uses.entry(local).or_default().push((inst, index, from));
                }
            }
        }
        places
    }

    /// Whether one instruction comes before another on every way to the second.
    fn precedes(&self, first: Inst, then: Inst) -> bool {
        let (Some(from), Some(to)) = (self.func.block_of(first), self.func.block_of(then)) else {
            return false;
        };
        if from == to {
            self.func.insts(from).find(|&inst| inst == first || inst == then) == Some(first)
        } else {
            self.dom.dominates(from, to)
        }
    }

    /// Whether running on from `from` can get to `to` without going through `through`.
    fn reaches(&self, from: Inst, to: Inst, through: Inst) -> bool {
        let Some(start) = self.func.block_of(from) else { return true };
        // What the rest of a block decides: `Some` when it meets one of the two, `None` when it
        // runs off the end and the walk goes on into the successors.
        let scan = |mut insts: Box<dyn Iterator<Item = Inst> + '_>| {
            insts.find(|&inst| inst == to || inst == through).map(|inst| inst == to)
        };
        let rest = self.func.insts(start).skip_while(move |&inst| inst != from).skip(1);
        if let Some(found) = scan(Box::new(rest)) {
            return found;
        }
        let mut seen: Set<Block> = Set::default();
        let mut work: Vec<Block> = self.cfg.successors(start).to_vec();
        while let Some(block) = work.pop() {
            if !seen.insert(block) {
                continue;
            }
            match scan(Box::new(self.func.insts(block))) {
                Some(true) => return true,
                Some(false) => {}
                None => work.extend_from_slice(self.cfg.successors(block)),
            }
        }
        false
    }

    /// The `alloc_size` of the function a call names, with the arguments of the call, for a direct
    /// call to a function the module has and that carries one.
    fn allocation(&self, call: Inst) -> Option<(AllocSize, &[Value])> {
        let data = &self.func[call];
        let Extra::Call(info) = data.extra else { return None };
        let name = self.func[info].callee?;
        let Some(SymbolRef::Func(callee)) = self.module.lookup(name) else { return None };
        let alloc = self.module[callee].attrs.alloc_size?;
        Some((alloc, &self.func[data.args]))
    }

    /// The address as what an allocator gave back with a constant number of bytes added, for an
    /// answer worked out at run time from the arguments of the call.
    ///
    /// A choice is a block parameter, and each way into it is followed the same way, or is a
    /// number the walk finds where it is not an allocator's result. A select is not followed.
    /// The arguments have to be as wide as the answer already, which `size_t` is in `malloc` and
    /// `kmalloc` and every allocator written in its terms. A narrower one has a signedness the IR
    /// does not remember, and widening it the wrong way would give a size that is not the one the
    /// program asked for, so that is answered as not known instead.
    ///
    /// `on` is the blocks whose parameter is being followed, which is how a loop is seen. A loop
    /// moving the address is not followed, since nothing could be passed round it.
    fn running(
        &self,
        value: Value,
        ask: Ask,
        size: Type,
        depth: u32,
        on: &mut Vec<Block>,
    ) -> Option<Running> {
        let depth = depth.checked_sub(1)?;
        if let Def::Param { block, index } = self.func[value].def {
            if on.contains(&block) {
                return None;
            }
            on.push(block);
            let joined = self.joined(block, index, ask, size, depth, on);
            on.pop();
            return joined;
        }
        let Def::Result { inst, .. } = self.func[value].def else { return None };
        let data = &self.func[inst];
        match data.opcode {
            Opcode::PtrAdd => {
                let args = &self.func[data.args];
                let (imm, ty) = self.number(*args.get(1)?, NUMBER)?;
                let step = u64::try_from(imm.signed(ty)).ok()?;
                // A member that may run past its end is asked about as the whole object, as
                // `left` asks it, or a way into a join behind it would answer with a member the
                // address is not in. mxs-dcp's `dcp_sha_import` clears `crypto_ahash_ctx (tfm)`,
                // the `__crt_ctx[]` of the `base` an inlined `crypto_ahash_tfm` gives back, and
                // `base` has nothing left there.
                let open =
                    data.extra.member() == Some(0) || matches!(data.extra, Extra::Trailing(_));
                let ask = if ask.closest && open { Ask { closest: false, ..ask } } else { ask };
                let mut running = self.running(args[0], ask, size, depth, on)?;
                running.offset = running.offset.checked_add(step)?;
                Some(running)
            }
            Opcode::Call => {
                let (alloc, args) = self.allocation(inst)?;
                let factors = factors(alloc, args)?.into_iter().map(|factor| self.put(factor));
                let factors: Vec<Value> = factors.collect();
                if factors.iter().any(|&factor| self.func[factor].ty != size) {
                    return None;
                }
                Some(Running { size: Size::Asked(factors), offset: 0 })
            }
            _ => None,
        }
    }

    /// The value a load reads back out of the local it was stored in, followed for as long as
    /// that is another such load, or the value itself where it is not one.
    ///
    /// An inlined `kmalloc_array_noprof` puts the size into `bytes` with `__builtin_mul_overflow`
    /// and reads it back on each of the two ways to `kmalloc_noprof`, so the two calls are asked
    /// for two loads. Both read what the one store put there, and that is the size the two ways
    /// agree on, as they do in gcc, which has no local by the time it answers.
    fn put(&self, value: Value) -> Value {
        let mut value = value;
        for _ in 0..DEPTH {
            let Def::Result { inst, .. } = self.func[value].def else { break };
            if self.func[inst].opcode != Opcode::Load {
                break;
            }
            let Some(&slot) = self.func[self.func[inst].args].first() else { break };
            match self.stored(slot, inst) {
                Some(stored) => value = stored,
                None => break,
            }
        }
        value
    }

    /// The size worked out when the program runs for a block parameter.
    ///
    /// `kmalloc_noprof` calls one of three allocators, and which one depends on a
    /// `__builtin_constant_p` of the size that is only settled after the questions are answered.
    /// All three are asked for the same size, so `kmalloc (size++, gfp)` in fortify_kunit's
    /// `fortify_test_alloc_size_kmalloc_dynamic` has that size, which is what gcc answers. Where
    /// every way in that can be taken agrees like that, and the arguments are there before the
    /// block, that is the size. Otherwise each way brings its own, and the arguments of each have
    /// to be there before the branch that brings it.
    fn joined(
        &self,
        block: Block,
        index: u32,
        ask: Ask,
        size: Type,
        depth: u32,
        on: &mut Vec<Block>,
    ) -> Option<Running> {
        let mut preds = self.cfg.predecessors(block).to_vec();
        preds.sort_unstable();
        preds.dedup();
        let mut ways = Vec::new();
        for pred in preds {
            let term = self.func.terminator(pred)?;
            let taken = self.taken(pred, block);
            for at in self.func.target_list(term).iter() {
                let call = self.func[at];
                if call.block != block {
                    continue;
                }
                let way = match taken {
                    false => None,
                    true => {
                        self.ways.set(self.ways.get().checked_sub(1)?);
                        let arg = *self.func[call.args].get(index as usize)?;
                        Some(self.way(arg, pred, ask, size, depth, on)?)
                    }
                };
                ways.push((term, at, way));
            }
        }
        let mut brought = ways.iter().filter_map(|(_, _, way)| way.as_ref());
        let first = brought.next()?;
        let before = |factor: &Value| match self.func[*factor].def {
            Def::Param { block: at, .. } => self.dom.dominates(at, block),
            Def::Result { inst, .. } => {
                self.func.block_of(inst).is_some_and(|at| self.dom.strictly_dominates(at, block))
            }
        };
        let agreed = match &first.size {
            Size::Asked(factors) => factors.iter().all(before),
            Size::Fixed(_) => true,
            Size::Joined(..) => false,
        };
        if agreed && brought.all(|way| way == first) {
            return Some(first.clone());
        }
        Some(Running { size: Size::Joined(block, ways), offset: 0 })
    }

    /// The size one way into a join brings, from the edge out of `pred`.
    fn way(
        &self,
        arg: Value,
        pred: Block,
        ask: Ask,
        size: Type,
        depth: u32,
        on: &mut Vec<Block>,
    ) -> Option<Running> {
        let way = match self.running(arg, ask, size, depth, on) {
            Some(way) => way,
            None => {
                let left = self.left(arg, ask, depth, &mut Vec::new()).ok().flatten()?;
                Running { size: Size::Fixed(left), offset: 0 }
            }
        };
        let there = |factor: &Value| match self.func[*factor].def {
            Def::Param { block: at, .. } => self.dom.dominates(at, pred),
            Def::Result { inst, .. } => {
                self.func.block_of(inst).is_some_and(|at| self.dom.dominates(at, pred))
            }
        };
        match &way.size {
            Size::Asked(factors) if !factors.iter().all(there) => None,
            _ => Some(way),
        }
    }

    /// The object every way to the address starts in, with its size, for an answer worked out
    /// when the program runs from how far into it the address is.
    ///
    /// The local has to be made before the question on every way to it, so that its address is
    /// there to subtract. A global is one whose size `extents::vouched` stands behind. For the
    /// closest member the object is the first member the walk passes the start of, which has to be
    /// worked out before the question in the same way. Where the walk passes none it is the whole
    /// object, which is no smaller than any member the address is in.
    ///
    /// What an allocator gave back has the size [`Walk::running`] works out for it, and that has
    /// to be there before the question too. btrfs's `create_chunk` copies each uuid with `memcpy
    /// (stripe->dev_uuid, ...)` at a `stripe` its loop moves through what `kzalloc (item_size)`
    /// gave back, and gcc keeps the field spanning WARN that checks it.
    fn within(&self, value: Value, question: Inst, ty: Type, ask: Ask) -> Option<Answer> {
        let object = self.object(value, ask, DEPTH, &mut Set::default()).ok()??;
        let size = match object {
            Object::Allocated(start) => {
                let there = match self.func[start].def {
                    Def::Param { block, .. } => {
                        self.func.block_of(question).is_some_and(|at| self.dom.dominates(block, at))
                    }
                    Def::Result { inst, .. } => self.precedes(inst, question),
                };
                if !there {
                    return None;
                }
                let whole = Ask { largest: true, closest: false };
                self.ways.set(WAYS);
                let running = self.running(start, whole, ty, DEPTH, &mut Vec::new())?;
                return (running.offset == 0).then_some(Answer::Allotted(start, running));
            }
            Object::Local(start) => {
                let Def::Result { inst, .. } = self.func[start].def else { return None };
                if !self.precedes(inst, question) {
                    return None;
                }
                let Extra::Mem(mem) = self.func[inst].extra else { return None };
                self.func[mem].size
            }
            Object::Member(start, size) => {
                let Def::Result { inst, .. } = self.func[start].def else { return None };
                if !self.precedes(inst, question) {
                    return None;
                }
                u64::from(size)
            }
            Object::Global(name) => {
                let Some(SymbolRef::Global(id)) = self.module.lookup(name) else { return None };
                let global = &self.module[id];
                if !vouched(global, self.pic) {
                    return None;
                }
                global.size
            }
        };
        Some(Answer::Within(object, size))
    }

    /// The object an address is in, as [`Found`] says.
    ///
    /// `seen` is each value the walk has been to. A value it comes to again adds nothing, since
    /// what it found the first time is in the answer already. This is also how a loop is seen. A
    /// walk that went to each value once for each way to it would take time that grows as a power
    /// of the number of joins, and the interpreter loop of lua has enough of them that it did not
    /// end.
    fn object(&self, value: Value, ask: Ask, depth: u32, seen: &mut Set<Value>) -> Found {
        let depth = depth.checked_sub(1).ok_or(())?;
        if !seen.insert(value) {
            return Ok(None);
        }
        if self.allocated(value, DEPTH) {
            return Ok(Some(Object::Allocated(value)));
        }
        match self.func[value].def {
            Def::Param { block, index } => {
                let preds = self.cfg.predecessors(block);
                if preds.is_empty() {
                    return Err(());
                }
                let mut all = Ok(None);
                for &pred in preds {
                    if !self.taken(pred, block) {
                        continue;
                    }
                    let term = self.func.terminator(pred).ok_or(())?;
                    for call in self.func.successors(term).collect::<Vec<_>>() {
                        if call.block != block {
                            continue;
                        }
                        let arg = *self.func[call.args].get(index as usize).ok_or(())?;
                        // One way that is not known is enough, and the rest need not be walked.
                        all = Ok(same(all, self.object(arg, ask, depth, seen))?);
                    }
                }
                all
            }
            Def::Result { inst, .. } => {
                let data = &self.func[inst];
                let args = &self.func[data.args];
                match data.opcode {
                    Opcode::Select => {
                        let (then, other) = (*args.get(1).ok_or(())?, *args.get(2).ok_or(())?);
                        let then = self.object(then, ask, depth, seen);
                        same(then, self.object(other, ask, depth, seen))
                    }
                    Opcode::PtrAdd => {
                        let base = *args.first().ok_or(())?;
                        let whole = Ask { closest: false, ..ask };
                        match data.extra {
                            _ if !ask.closest => self.object(base, ask, depth, seen),
                            // A member that may run past its end is in the whole object.
                            Extra::Member(0) | Extra::Array(0) | Extra::Counted(_) => {
                                self.object(base, whole, depth, seen)
                            }
                            Extra::Member(size) | Extra::Array(size) => {
                                Ok(Some(Object::Member(value, size)))
                            }
                            Extra::Trailing(size) if self.declared(base) || self.indexed(base) => {
                                Ok(Some(Object::Member(value, size)))
                            }
                            Extra::Trailing(_) => self.object(base, whole, depth, seen),
                            _ => self.object(base, ask, depth, seen),
                        }
                    }
                    Opcode::Alloca if args.is_empty() => Ok(Some(Object::Local(value))),
                    Opcode::GlobalAddr => {
                        let Extra::Symbol(name) = data.extra else { return Err(()) };
                        Ok(Some(Object::Global(name)))
                    }
                    Opcode::Load => {
                        if let Some(name) = self.pointed(inst) {
                            return Ok(Some(Object::Global(name)));
                        }
                        let put = self.stored(*args.first().ok_or(())?, inst).ok_or(())?;
                        self.object(put, ask, depth, seen)
                    }
                    Opcode::Call => self.object(self.into(inst).ok_or(())?, ask, depth, seen),
                    _ => Err(()),
                }
            }
        }
    }

    /// Whether the value is a null pointer.
    fn null(&self, value: Value) -> bool {
        let value = match self.func[value].def {
            Def::Result { inst, .. } if self.func[inst].opcode == Opcode::IntToPtr => {
                let Some(&int) = self.func[self.func[inst].args].first() else { return false };
                int
            }
            _ => value,
        };
        self.number(value, NUMBER).is_some_and(|(imm, _)| imm.unsigned() == 0)
    }

    /// Whether `at` is only reached on the way out of a branch that has found `value` is not a
    /// null pointer.
    fn guarded(&self, value: Value, at: Inst) -> bool {
        let Some(mut block) = self.func.block_of(at) else { return false };
        for _ in 0..DEPTH {
            if let &[pred] = self.cfg.predecessors(block) {
                if self.not_null(pred, block, value) {
                    return true;
                }
            }
            match self.dom.immediate_dominator(block) {
                Some(up) => block = up,
                None => return false,
            }
        }
        false
    }

    /// Whether the edge from `pred` to `block` is the way a branch takes where `value` is not a
    /// null pointer.
    ///
    /// The test may be turned round by an `xor` with true, which is how `if (!r)` comes out of
    /// lowering. tls's `tls_get_rec` returns `NULL` under one and `tls_split_open_record` tests
    /// what it got the same way, and gcc keeps the field spanning WARN of the copy into the
    /// record once it is past that.
    fn not_null(&self, pred: Block, block: Block, value: Value) -> bool {
        let Some(term) = self.func.terminator(pred) else { return false };
        if self.func[term].opcode != Opcode::BrIf {
            return false;
        }
        let Some(&cond) = self.func[self.func[term].args].first() else { return false };
        let (mut cond, mut flipped) = (cond, false);
        let inst = loop {
            let Def::Result { inst, .. } = self.func[cond].def else { return false };
            let test = &self.func[inst];
            let &[one, other] = &self.func[test.args][..] else { return false };
            if test.opcode != Opcode::Xor || self.func[cond].ty != Type::I1 {
                break inst;
            }
            let all =
                |v: Value| self.number(v, NUMBER).is_some_and(|(imm, _)| imm.unsigned() & 1 == 1);
            cond = match (all(one), all(other)) {
                (_, true) => one,
                (true, false) => other,
                (false, false) => return false,
            };
            flipped = !flipped;
        };
        let test = &self.func[inst];
        let (Opcode::ICmp, Extra::IntPred(pred)) = (test.opcode, test.extra) else { return false };
        let &[one, other] = &self.func[test.args][..] else { return false };
        if !(one == value && self.null(other) || other == value && self.null(one)) {
            return false;
        }
        let mut calls: Vec<Block> = self.func.successors(term).map(|call| call.block).collect();
        if flipped {
            calls.reverse();
        }
        match (pred, calls.as_slice()) {
            (_, &[then, other]) if then == other => false,
            (IntPred::Ne, &[then, _]) => then == block,
            (IntPred::Eq, &[_, other]) => other == block,
            _ => false,
        }
    }

    /// Whether the value is what an allocator gave back, or a join every way into which that can
    /// be taken brings that. `kmalloc_noprof` calls one of three allocators, and the address a
    /// loop then moves starts at the join of the three.
    fn allocated(&self, value: Value, depth: u32) -> bool {
        let Some(depth) = depth.checked_sub(1) else { return false };
        match self.func[value].def {
            Def::Result { inst, .. } => {
                self.func[inst].opcode == Opcode::Call && self.allocation(inst).is_some()
            }
            Def::Param { block, index } => {
                let preds = self.cfg.predecessors(block);
                let mut brought = false;
                for &pred in preds {
                    if self.dom.dominates(block, pred) {
                        return false;
                    }
                    if !self.taken(pred, block) {
                        continue;
                    }
                    let Some(term) = self.func.terminator(pred) else { return false };
                    for call in self.func.successors(term).collect::<Vec<_>>() {
                        if call.block != block {
                            continue;
                        }
                        let Some(&arg) = self.func[call.args].get(index as usize) else {
                            return false;
                        };
                        if !self.allocated(arg, depth) {
                            return false;
                        }
                        brought = true;
                    }
                }
                brought
            }
        }
    }

    /// The argument a library call gives back an address in the object of, for the calls that do
    /// that whatever they are given.
    ///
    /// A copy or a fill gives back where it wrote, and the `p` spellings give back the end of what
    /// they wrote. `strchr (s, 0)` gives back the end of `s`. A `strchr` for any other character
    /// may give back a null pointer, and gcc does not follow it either.
    fn into(&self, call: Inst) -> Option<Value> {
        let library = self.library?;
        let data = &self.func[call];
        let Extra::Call(info) = data.extra else { return None };
        let callee = self.func[info].callee?;
        // The front end calls a `_chk` function that a fortified header names without declaring
        // it, so the module has nothing for that name.
        let spelled = match self.module.lookup(callee) {
            Some(SymbolRef::Func(id)) => self.module[id].spelled.unwrap_or(callee),
            None => callee,
            Some(_) => return None,
        };
        let name = self.names.resolve(spelled);
        if library.iter().any(|it| it == name) {
            return None;
        }
        let args = &self.func[data.args];
        match name {
            "memcpy" | "memmove" | "memset" | "mempcpy" | "strcpy" | "stpcpy" | "strncpy"
            | "stpncpy" | "strcat" | "strncat" | "__memcpy_chk" | "__memmove_chk"
            | "__memset_chk" | "__mempcpy_chk" | "__strcpy_chk" | "__stpcpy_chk"
            | "__strncpy_chk" | "__stpncpy_chk" | "__strcat_chk" | "__strncat_chk" => {
                args.first().copied()
            }
            "strchr" | "index" => {
                let (imm, _) = self.number(*args.get(1)?, NUMBER)?;
                (imm.unsigned() == 0).then_some(args[0])
            }
            _ => None,
        }
    }
}

/// What one walk of [`Walk::object`] found: `Err` where the address may be in more than one object
/// or in one the walk cannot see, `Ok(None)` where the only way to it is round a loop back to
/// itself, and otherwise the object.
type Found = Result<Option<Object>, ()>;

/// Two answers for one choice, which have to be the same object.
fn same(one: Found, other: Found) -> Found {
    match (one?, other?) {
        (Some(one), Some(other)) if one != other => Err(()),
        (one, other) => Ok(one.or(other)),
    }
}

/// The arguments of a call that `alloc_size` says multiply to the size of what it returns, and
/// nothing for a call that has fewer arguments than the attribute counts.
fn factors(alloc: AllocSize, args: &[Value]) -> Option<Vec<Value>> {
    let mut factors = Vec::with_capacity(2);
    for number in [Some(alloc.size), alloc.count].into_iter().flatten() {
        factors.push(*args.get(usize::from(number).checked_sub(1)?)?);
    }
    Some(factors)
}

/// Two answers for one choice, as the kind asks for them to be put together.
fn both(one: Left, other: Left, largest: bool) -> Left {
    Ok(match (one?, other?) {
        (Some(one), Some(other)) if largest => Some(one.max(other)),
        (Some(one), Some(other)) => Some(one.min(other)),
        (Some(one), None) | (None, Some(one)) => Some(one),
        (None, None) => None,
    })
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;

    use super::*;

    /// What every fixture below starts with, which is the target the widths are of.
    const HEAD: &str = "\
; ModuleID = 't.c'
; format 0
target triple = \"x86_64-unknown-linux-gnu\"
target datalayout = \"e-p:64:64-i64:64-f80:128-S128\"
";

    /// The numbers each call to `@use` in the module was given once every question is answered,
    /// in the order the calls are written.
    fn answers(body: &str, look: bool) -> Vec<i128> {
        let mut names = Interner::new();
        let text = format!("{HEAD}{body}");
        let mut module = rucc_ir::parse(&text, &mut names).expect("the fixture parses");
        answer(&mut module, &names, Some(&[]), Pic::Executable, look);
        if let Err(errors) = rucc_ir::verify(&module, &names) {
            panic!("the answers left invalid IR, {errors:?}\n{}", rucc_ir::print(&module, &names));
        }
        let mut found = Vec::new();
        for id in module.funcs() {
            let func = &module[id];
            for block in func.blocks() {
                for inst in func.insts(block) {
                    assert_ne!(func[inst].opcode, Opcode::ObjectSize, "a question was left");
                    if func[inst].opcode != Opcode::Call {
                        continue;
                    }
                    let &[value] = &func[func[inst].args] else { continue };
                    let Def::Result { inst: def, .. } = func[value].def else { continue };
                    let Extra::Imm(imm) = func[def].extra else { continue };
                    found.push(func[imm].signed(func[value].ty));
                }
            }
        }
        found
    }

    /// What an allocator gave back is the product of the arguments its `alloc_size` names where
    /// they are constants, for both spellings. Where they are not, the dynamic spelling is
    /// answered with the arguments as they are when the program runs, less what was added to the
    /// address, and the other is not known. The numbers are what gcc 16 answers at `-O2`.
    #[test]
    fn what_an_allocator_gave_back_is_what_its_alloc_size_says() {
        let body = "
func @my(i64) -> ptr, linkage(external), attrs(alloc_size=1);
func @my2(i64, i64) -> ptr, linkage(external), attrs(alloc_size=1, alloc_count=2);
func @plain(i64) -> ptr, linkage(external);

func @f(i64), linkage(external) {
block0(%0: i64):
    %1 = iconst.i64 10
    %2 = call @my(%1) : (i64) -> ptr
    %3 = object_size.i64 %2, kind 0
    call @use(%3) : (i64)
    %4 = iconst.i64 4
    %5 = ptr_add %2, %4
    %6 = object_size.i64 %5, kind 2
    call @use(%6) : (i64)
    %7 = iconst.i64 3
    %8 = iconst.i64 5
    %9 = call @my2(%7, %8) : (i64, i64) -> ptr
    %10 = object_size.i64 %9, kind 4
    call @use(%10) : (i64)
    %11 = call @plain(%1) : (i64) -> ptr
    %12 = object_size.i64 %11, kind 0
    call @use(%12) : (i64)
    %13 = call @my(%0) : (i64) -> ptr
    %14 = object_size.i64 %13, kind 0
    call @use(%14) : (i64)
    %15 = ptr_add %13, %4
    %16 = object_size.i64 %15, kind 4
    call @use(%16) : (i64)
    return
}
";
        // Every call with one constant argument is read back, so the two calls given ten are
        // among them. The last question is not a constant when looked at, so it is not.
        assert_eq!(answers(body, true), [10, 10, 6, 15, 10, -1, -1]);
        assert_eq!(answers(body, false), [10, -1, 0, -1, 10, -1, -1, -1]);
        let mut names = Interner::new();
        let mut module = rucc_ir::parse(&format!("{HEAD}{body}"), &mut names).expect("parses");
        answer(&mut module, &names, Some(&[]), Pic::Executable, true);
        let text = rucc_ir::print(&module, &names);
        assert!(text.contains("icmp ugt %0, "), "{text}");
        assert!(text.contains("= sub %0, "), "{text}");
    }

    /// A choice of two allocators asked for the same size has that size when the program runs,
    /// which is what `kmalloc_noprof` is before its `__builtin_constant_p` is settled. Two asked
    /// for different sizes each pass their own to the join, through a parameter it is given.
    #[test]
    fn a_choice_of_allocators_given_one_size_has_that_size() {
        let body = "
global @g : bytes 8 = { zero 8 }, align 8, linkage(external)
func @my(i64) -> ptr, linkage(external), attrs(alloc_size=1);
func @cache(ptr, i64) -> ptr, linkage(external), attrs(alloc_size=2);

func @f(i64, i64, i32), linkage(external) {
block0(%0: i64, %1: i64, %2: i32):
    %3 = iconst.i32 0
    %4 = icmp ne %2, %3
    br_if %4, block1, block2

block1:
    %5 = call @my(%0) : (i64) -> ptr
    jump block3(%5)

block2:
    %6 = global_addr @g
    %7 = call @cache(%6, %0) : (ptr, i64) -> ptr
    jump block3(%7)

block3(%8: ptr):
    %9 = object_size.i64 %8, kind 5
    call @use(%9) : (i64)
    br_if %4, block4, block5

block4:
    %10 = call @my(%1) : (i64) -> ptr
    jump block6(%10)

block5:
    jump block6(%8)

block6(%11: ptr):
    %12 = object_size.i64 %11, kind 5
    call @use(%12) : (i64)
    return
}
";
        assert_eq!(answers(body, true), []);
        let mut names = Interner::new();
        let mut module = rucc_ir::parse(&format!("{HEAD}{body}"), &mut names).expect("parses");
        answer(&mut module, &names, Some(&[]), Pic::Executable, true);
        let text = rucc_ir::print(&module, &names);
        assert!(text.contains("call @use(%0)"), "{text}");
        assert!(text.contains("jump block6(%9, %1)"), "{text}");
        assert!(text.contains("jump block6(%8, %0)"), "{text}");
    }

    /// A branch on a question already answered `(size_t)-1` is settled by that answer, the way
    /// `p_size < size` is in 6.12's `kmemdup_noprof`, so only the allocation on the way it is
    /// false is what comes back. The other way round the comparison could go either way, and each
    /// way brings the size it asked for, which is what gcc answers.
    #[test]
    fn a_comparison_with_an_answer_as_large_as_it_goes_is_settled() {
        let body = |test: &str| {
            format!(
                "
func @cache(ptr, i64) -> ptr, linkage(external), attrs(alloc_size=2);

func @f(ptr, i64), linkage(external) {{
block0(%0: ptr, %1: i64):
    %2 = object_size.i64 %0, kind 4
    %3 = icmp {test}
    br_if %3, block1, block2

block1:
    %4 = iconst.i64 0
    %5 = call @cache(%0, %4) : (ptr, i64) -> ptr
    jump block3(%5)

block2:
    %6 = call @cache(%0, %1) : (ptr, i64) -> ptr
    jump block3(%6)

block3(%7: ptr):
    %8 = object_size.i64 %7, kind 5
    call @use(%8) : (i64)
    return
}}
"
            )
        };
        let print = |body: &str| {
            let mut names = Interner::new();
            let mut module = rucc_ir::parse(&format!("{HEAD}{body}"), &mut names).expect("parses");
            answer(&mut module, &names, Some(&[]), Pic::Executable, true);
            rucc_ir::print(&module, &names)
        };
        let text = print(&body("ult %2, %1"));
        assert!(text.contains("call @use(%1)"), "{text}");
        let text = print(&body("ult %1, %2"));
        assert!(text.contains("jump block3(%5, %4)"), "{text}");
        assert!(text.contains("jump block3(%6, %1)"), "{text}");
    }

    /// The ways into a join may bring an allocation and a local, and what is added to the address
    /// after the join is taken off what either brought, as gcc 15 answers it at `-O2` for
    /// `p = c ? malloc (n) : buf + 8` with a 32 byte `buf`. A step the walk cannot count is
    /// measured on its way, as gcc 13 does for `buf + n`. A way that brings nothing known, which is
    /// a call to something that is not an allocator, leaves the whole join not known.
    #[test]
    fn each_way_into_a_join_brings_its_own_size() {
        let body = |other: &str| {
            format!(
                "
func @my(i64) -> ptr, linkage(external), attrs(alloc_size=1);
func @plain(i64) -> ptr, linkage(external);

func @f(i64, i32), linkage(external) {{
block0(%0: i64, %1: i32):
    %2 = alloca, size 32, align 8
    %3 = iconst.i32 0
    %4 = icmp ne %1, %3
    %5 = iconst.i64 8
    br_if %4, block1, block2

block1:
    %6 = call @my(%0) : (i64) -> ptr
    jump block3(%6)

block2:
    %7 = {other}
    jump block3(%7)

block3(%8: ptr):
    %9 = iconst.i64 4
    %10 = ptr_add %8, %9
    %11 = object_size.i64 %10, kind 4
    call @use(%11) : (i64)
    %12 = object_size.i64 %10, kind 0
    call @use(%12) : (i64)
    return
}}
"
            )
        };
        let mut names = Interner::new();
        let text = format!("{HEAD}{}", body("ptr_add %2, %5"));
        let mut module = rucc_ir::parse(&text, &mut names).expect("parses");
        answer(&mut module, &names, Some(&[]), Pic::Executable, true);
        let text = rucc_ir::print(&module, &names);
        assert!(text.contains("jump block3(%6, %0)"), "{text}");
        assert!(text.contains("iconst.i64 24"), "{text}");
        assert!(text.contains("icmp ugt"), "{text}");
        assert_eq!(answers(&body("ptr_add %2, %5"), true), [-1]);
        assert_eq!(answers(&body("ptr_add %2, %0"), true), [-1]);
        assert_eq!(answers(&body("call @plain(%0) : (i64) -> ptr"), true), [-1, -1]);
    }

    /// An allocator's result put in a local once and read back is what the allocator gave, which
    /// is the shape a `__free (kfree)` variable has. A second store, or the slot's address going
    /// anywhere but a load or a store, and it is not known.
    #[test]
    fn an_address_read_back_out_of_a_local_is_what_the_last_store_put_there() {
        let body = |extra: &str| {
            format!(
                "
func @my(i64) -> ptr, linkage(external), attrs(alloc_size=1);
func @keep(ptr), linkage(external);

func @f(i64), linkage(external) {{
block0(%0: i64):
    %1 = alloca, size 8, align 8
    %2 = iconst.i64 768
    %3 = call @my(%2) : (i64) -> ptr
    %4 = iconst.i64 16
    %5 = call @my(%4) : (i64) -> ptr
    store %3 -> %1, align 8
{extra}    %6 = load.ptr %1, align 8
    %7 = object_size.i64 %6, kind 0
    call @use(%7) : (i64)
    return
}}
"
            )
        };
        assert_eq!(answers(&body(""), true), [768, 16, 768]);
        // A later store is the one the load reads.
        assert_eq!(answers(&body("    store %5 -> %1, align 8\n"), true), [768, 16, 16]);
        assert_eq!(answers(&body("    call @keep(%1) : (ptr)\n"), true), [768, 16, -1]);
    }

    /// The address of a buffer put in one field of a local and read back out, the way iommufd
    /// hands `ucmd.cmd` to `copy_struct_from_user`, is the buffer, though the local goes to a call
    /// later on. A call that gets the local before the load leaves it unknown, and so does a store
    /// to part of the field.
    #[test]
    fn an_address_read_back_out_of_a_field_of_a_local_is_what_was_stored_there() {
        let body = |extra: &str| {
            format!(
                "
func @keep(ptr), linkage(external);

func @f(ptr), linkage(external) {{
block0(%0: ptr):
    %1 = alloca, size 24, align 8
    %2 = alloca, size 72, align 8
    %3 = iconst.i8 0
    %4 = iconst.i64 16
    %5 = iconst.i64 20
    %6 = iconst.i32 0
    %7 = ptr_add %1, %5
    memset %1, %3, size 24, align 8
    %8 = ptr_add %1, %4
    store %2 -> %8, align 8
    store %0 -> %1, align 8
{extra}    %9 = ptr_add %1, %4
    %10 = load.ptr %9, align 8
    %11 = object_size.i64 %10, kind 1
    call @use(%11) : (i64)
    call @keep(%1) : (ptr)
    return
}}
"
            )
        };
        assert_eq!(answers(&body(""), true), [72]);
        assert_eq!(answers(&body("    call @keep(%1) : (ptr)\n"), true), [-1]);
        assert_eq!(answers(&body("    store %6 -> %7, align 4\n"), true), [-1]);
    }

    /// A store on one arm of a branch can be what the load reads, so the store before the branch
    /// does not answer for it, even though that one comes before the load on every way to it.
    #[test]
    fn a_store_that_can_reach_the_load_another_way_leaves_it_unknown() {
        let body = "
func @my(i64) -> ptr, linkage(external), attrs(alloc_size=1);
func @other() -> ptr, linkage(external);

func @f(i1), linkage(external) {
block0(%0: i1):
    %1 = alloca, size 8, align 8
    %2 = iconst.i64 768
    %3 = call @my(%2) : (i64) -> ptr
    store %3 -> %1, align 8
    br_if %0, block1, block2

block1:
    %4 = call @other() : () -> ptr
    store %4 -> %1, align 8
    jump block2

block2:
    %5 = load.ptr %1, align 8
    %6 = object_size.i64 %5, kind 0
    call @use(%6) : (i64)
    return
}
";
        assert_eq!(answers(body, true), [768, -1]);
    }

    /// `kmalloc_array` on two constants, inlined twice into one slot for `bytes` that
    /// `-ftrivial-auto-var-init=zero` clears first. The overflow check cannot fail, so its `NULL`
    /// is never what comes back, and each load reads the store in front of it.
    #[test]
    fn a_size_from_an_overflow_check_through_a_shared_slot_is_followed() {
        let body = |second: &str| {
            format!(
                "
func @my(i64) -> ptr, linkage(external), attrs(alloc_size=1);

func @f(i32), linkage(external) {{
block0(%0: i32):
    %1 = alloca, size 8, align 8
    %2 = iconst.i64 256
    %3 = iconst.i64 3
    %4 = iconst.i8 0
    %5 = iconst.i32 0
    %6 = icmp ne %0, %5
    br_if %6, block1, block4

block1:
    memset %1, %4, size 8, align 8
    %7, %8 = umul_overflow.(i64, i1) %2, %3
    store %7 -> %1, align 8
    br_if %8, block2, block3

block2:
    %9 = iconst.i64 0
    %10 = inttoptr.ptr %9
    jump block5(%10)

block3:
    %11 = load.i64 %1, align 8
    %12 = call @my(%11) : (i64) -> ptr
    jump block5(%12)

block4:
    memset %1, %4, size 8, align 8
    %13 = iconst.i64 {second}
    store %13 -> %1, align 8
    %14 = load.i64 %1, align 8
    %15 = call @my(%14) : (i64) -> ptr
    %16 = object_size.i64 %15, kind 0
    call @use(%16) : (i64)
    return

block5(%17: ptr):
    %18 = object_size.i64 %17, kind 0
    call @use(%18) : (i64)
    return
}}
"
            )
        };
        assert_eq!(answers(&body("16"), true), [16, 768]);
    }

    /// A pointer chosen by a branch between a local and a global has the larger of what the two
    /// have left for the first kind and the smaller for the third.
    #[test]
    fn a_choice_of_two_objects_is_the_larger_or_the_smaller_of_what_each_has_left() {
        let body = "
global @g : bytes 32 = { zero 32 }, align 1, linkage(external)

func @f(i32), linkage(external) {
block0(%0: i32):
    %1 = alloca, size 20, align 16
    %2 = iconst.i32 0
    %3 = icmp ne %0, %2
    br_if %3, block1, block2

block1:
    %4 = iconst.i32 5
    %5 = sext.i64 %4
    %6 = ptr_add %1, %5
    jump block3(%6)

block2:
    %7 = global_addr @g
    %8 = iconst.i64 4
    %9 = ptr_add %7, %8
    jump block3(%9)

block3(%10: ptr):
    %11 = object_size.i64 %10, kind 0
    call @use(%11) : (i64)
    %12 = object_size.i64 %10, kind 1
    call @use(%12) : (i64)
    %13 = object_size.i64 %10, kind 2
    call @use(%13) : (i64)
    %14 = object_size.i64 %10, kind 3
    call @use(%14) : (i64)
    return
}
";
        // The second kind is the whole object's answer, which is no smaller than any member's,
        // and the fourth is not known here at all.
        assert_eq!(answers(body, true), [28, 28, 15, 0]);
        // And at `-O0` none of it is looked at.
        assert_eq!(answers(body, false), [-1, -1, 0, 0]);
    }

    /// The dynamic spelling of the same choice is what is left on the way the address came, the
    /// PHI of sizes gcc 13 builds, where the other spelling is the larger or the smaller. Ways that
    /// bring the same size are still that one number.
    #[test]
    fn a_choice_of_two_objects_asked_when_the_program_runs_is_what_its_way_has_left() {
        let body = |other: &str| {
            format!(
                "
global @g : bytes 32 = {{ zero 32 }}, align 1, linkage(external)

func @f(i32), linkage(external) {{
block0(%0: i32):
    %1 = alloca, size 20, align 16
    %2 = iconst.i32 0
    %3 = icmp ne %0, %2
    br_if %3, block1, block2

block1:
    %4 = iconst.i64 5
    %5 = ptr_add %1, %4
    jump block3(%5)

block2:
    %6 = global_addr @g
    %7 = iconst.i64 {other}
    %8 = ptr_add %6, %7
    jump block3(%8)

block3(%9: ptr):
    %10 = object_size.i64 %9, kind 4
    call @use(%10) : (i64)
    %11 = object_size.i64 %9, kind 5
    call @use(%11) : (i64)
    %12 = object_size.i64 %9, kind 6
    call @use(%12) : (i64)
    %13 = object_size.i64 %9, kind 0
    call @use(%13) : (i64)
    return
}}
"
            )
        };
        let text = printed(&body("4"), Some(&[]));
        let joined =
            text.lines().any(|line| line.starts_with("block3(") && line.ends_with(": i64):"));
        assert!(joined, "the join is given a parameter, {text}");
        assert!(text.contains("iconst.i64 15") && text.contains("iconst.i64 28"), "{text}");
        assert_eq!(answers(&body("4"), true), [28]);
        assert_eq!(answers(&body("17"), true), [15, 15, 15, 15]);
    }

    /// A way into a choice that a `switch` took and a later `switch` on the same value turns away
    /// from the question adds nothing to the choice, as gcc has threaded the two by the time it
    /// answers. The way that leaves the pointer null is the default of the first switch, and the
    /// second only goes on to the question for a case the first one has. Each switch widens the
    /// value on its own, as the front end leaves it.
    #[test]
    fn a_way_a_later_switch_on_the_same_value_turns_away_is_not_part_of_the_choice() {
        let body = |cases: &str| {
            format!(
                "
func @f(i8), linkage(external) {{
block0(%0: i8):
    %1 = alloca, size 20, align 16
    %2 = iconst.i64 0
    %3 = inttoptr.ptr %2
    %4 = zext.i32 %0
    switch %4, block1(%3), [0 => block2, 1 => block3]

block1(%5: ptr):
    %6 = zext.i32 %0
    switch %6, block4, [{cases}]

block2:
    %7 = iconst.i64 5
    %8 = ptr_add %1, %7
    jump block1(%8)

block3:
    %9 = iconst.i64 12
    %10 = ptr_add %1, %9
    jump block1(%10)

block4:
    return

block5:
    %11 = object_size.i64 %5, kind 5
    call @use(%11) : (i64)
    %12 = object_size.i64 %5, kind 4
    call @use(%12) : (i64)
    return
}}
"
            )
        };
        let text = printed(&body("0 => block5, 1 => block5"), Some(&[]));
        let joined =
            text.lines().any(|line| line.starts_with("block1(") && line.ends_with(": i64):"));
        assert!(joined, "the join is given a parameter, {text}");
        assert!(text.contains("iconst.i64 15") && text.contains("iconst.i64 8"), "{text}");
        assert_eq!(answers(&body("0 => block5, 1 => block5"), true), []);
        assert_eq!(answers(&body("0 => block5, 7 => block5"), true), [-1, -1]);
    }

    /// A pointer carried round a loop and replaced on some trips is the choice of every address it
    /// was given, and the trip that leaves it alone adds nothing to the choice.
    #[test]
    fn a_pointer_a_loop_leaves_alone_is_what_it_was_given() {
        let body = "
func @f(i32), linkage(external) {
block0(%0: i32):
    %1 = alloca, size 20, align 16
    %2 = iconst.i32 0
    jump block1(%1, %2)

block1(%3: ptr, %4: i32):
    %5 = icmp eq %4, %0
    %6 = iconst.i64 7
    %7 = ptr_add %1, %6
    %8 = select.ptr %5, %7, %3
    %9 = iconst.i32 1
    %10 = add %4, %9
    %11 = icmp slt %10, %0
    br_if %11, block1(%8, %10), block2

block2:
    %12 = object_size.i64 %8, kind 0
    call @use(%12) : (i64)
    %13 = object_size.i64 %8, kind 2
    call @use(%13) : (i64)
    return
}
";
        assert_eq!(answers(body, true), [20, 13]);
    }

    /// A pointer moved forward each time round a loop has less and less left, so the largest is
    /// where it started and there is no smallest. Moved backward there is no largest either, since
    /// it may have more left than anything the walk saw.
    #[test]
    fn a_pointer_a_loop_moves_is_known_only_where_that_can_only_leave_less() {
        let forward = "
func @f(i32), linkage(external) {
block0(%0: i32):
    %1 = alloca, size 20, align 16
    %2 = iconst.i32 0
    jump block1(%1, %2)

block1(%3: ptr, %4: i32):
    %5 = iconst.i64 STEP
    %6 = ptr_add %3, %5
    %7 = iconst.i32 1
    %8 = add %4, %7
    %9 = icmp slt %8, %0
    br_if %9, block1(%6, %8), block2

block2:
    %10 = object_size.i64 %6, kind 0
    call @use(%10) : (i64)
    %11 = object_size.i64 %6, kind 2
    call @use(%11) : (i64)
    return
}
";
        assert_eq!(answers(&forward.replace("STEP", "1"), true), [19, 0]);
        assert_eq!(answers(&forward.replace("STEP", "-1"), true), [-1, 0]);
    }

    /// The dynamic spelling of the largest answer about a pointer a loop moves forward is the room
    /// it has on the trip the question is asked, worked out when the program runs, and the static
    /// spelling stays the room where it starts. Where the loop leaves the pointer alone both are
    /// what the walk found. gcc 15 answers defxx's `p_addr` this way at `-O2`.
    #[test]
    fn a_pointer_a_loop_moves_has_the_room_it_has_on_that_trip() {
        let body = "
func @f(i32, ptr), linkage(external) {
block0(%0: i32, %1: ptr):
    %2 = iconst.i64 4
    %3 = ptr_add %1, %2, member 32
    %4 = iconst.i32 0
    jump block1(%3, %4)

block1(%5: ptr, %6: i32):
    %7 = object_size.i64 %5, kind 5
    call @use(%7) : (i64)
    %8 = object_size.i64 %5, kind 1
    call @use(%8) : (i64)
    %9 = iconst.i64 STEP
    %10 = ptr_add %5, %9
    %11 = iconst.i32 1
    %12 = add %6, %11
    %13 = icmp slt %12, %0
    br_if %13, block1(%10, %12), block2

block2:
    return
}
";
        let moved = body.replace("STEP", "8");
        assert_eq!(answers(&moved, true), [32]);
        let text = printed(&moved, Some(&[]));
        assert_eq!(text.matches("ptrtoint").count(), 2, "{text}");
        assert_eq!(text.matches("icmp ugt").count(), 1, "{text}");
        assert!(text.contains("iconst.i64 32"), "{text}");
        assert_eq!(answers(&body.replace("STEP", "0"), true), [32, 32]);
    }

    /// A step to a member that may run past its end, behind a join that brings a member the
    /// walk knows the length of, is asked about as the whole object, which through a pointer is
    /// not known. That is mxs-dcp's `crypto_ahash_ctx (tfm)` under gcc 12, where `base` is 128
    /// bytes and `__crt_ctx[]` starts at its end, and gcc answers both spellings with -1. A plain
    /// step leaves the question about `base`, which has nothing left.
    #[test]
    fn a_member_that_runs_past_its_end_behind_a_join_is_the_whole_object() {
        let body = "
func @f(ptr), linkage(external) {
block0(%0: ptr):
    %1 = iconst.i64 128
    %2 = ptr_add %0, %1, member 128
    jump block1(%2)

block1(%3: ptr):
    %4 = iconst.i64 128
    %5 = ptr_add %3, %4MARK
    %6 = object_size.i64 %5, kind 5
    call @use(%6) : (i64)
    %7 = object_size.i64 %5, kind 1
    call @use(%7) : (i64)
    return
}
";
        assert_eq!(answers(&body.replace("MARK", ", array 0"), true), [-1, -1]);
        assert_eq!(answers(&body.replace("MARK", ", trailing 8"), true), [-1, -1]);
        assert_eq!(answers(&body.replace("MARK", ""), true), [0, 0]);
    }

    /// A pointer a loop moves through what an allocator gave back has, for the dynamic spelling,
    /// the size the allocator was asked for less how far the loop has moved it, and the closest
    /// member is no more than that, as gcc 16 answers for btrfs's `stripe` in `create_chunk`.
    /// Both ways out of a choice of allocators are the start, as `kmalloc_noprof` makes it.
    #[test]
    fn a_pointer_a_loop_moves_through_an_allocation_has_what_the_allocator_was_asked_for_left() {
        let body = |other: &str| {
            format!(
                "
func @my(i64) -> ptr, linkage(external), attrs(alloc_size=1);
func @other(i64) -> ptr, linkage(external), attrs(alloc_size=1);

func @f(i32, i64, i1), linkage(external) {{
block0(%0: i32, %1: i64, %2: i1):
    br_if %2, block1, block2

block1:
    %3 = call @my(%1) : (i64) -> ptr
    jump block3(%3)

block2:
    %4 = call @{other}(%1) : (i64) -> ptr
    jump block3(%4)

block3(%5: ptr):
    %6 = iconst.i64 16
    %7 = ptr_add %5, %6
    %8 = iconst.i32 0
    jump block4(%7, %8)

block4(%9: ptr, %10: i32):
    %11 = object_size.i64 %9, kind 4
    call @use(%11) : (i64)
    %12 = iconst.i64 8
    %13 = ptr_add %9, %12, member 16
    %14 = object_size.i64 %13, kind 5
    call @use(%14) : (i64)
    %15 = object_size.i64 %9, kind 0
    call @use(%15) : (i64)
    %16 = object_size.i64 %13, kind 1
    call @use(%16) : (i64)
    %17 = iconst.i64 32
    %18 = ptr_add %9, %17
    %19 = iconst.i32 1
    %20 = add %10, %19
    %21 = icmp slt %20, %0
    br_if %21, block4(%18, %20), block5

block5:
    return
}}
"
            )
        };
        for other in ["my", "other"] {
            let body = body(other);
            assert_eq!(answers(&body, true), [-1, 16], "{other}");
            let text = printed(&body, Some(&[]));
            assert_eq!(text.matches("ptrtoint").count(), 4, "{text}");
            assert_eq!(text.matches("icmp ugt").count(), 3, "{text}");
        }
    }

    /// The module once every question is answered, printed, with `library` as [`answer`] takes it.
    fn printed(body: &str, library: Option<&[String]>) -> String {
        let mut names = Interner::new();
        let mut module = rucc_ir::parse(&format!("{HEAD}{body}"), &mut names).expect("parses");
        answer(&mut module, &names, library, Pic::Executable, true);
        if let Err(errors) = rucc_ir::verify(&module, &names) {
            panic!("the answers left invalid IR, {errors:?}\n{}", rucc_ir::print(&module, &names));
        }
        rucc_ir::print(&module, &names)
    }

    /// The dynamic spelling of a kind that asks for the largest answer, about an address every way
    /// to which starts at one fixed local or one global, is the size of that object less how far
    /// in the address is, worked out when the program runs. The walk goes through a step of any
    /// count, a copy that gives back where it wrote and `strchr (s, 0)`, and round a loop. The copy
    /// here is a `_chk` call with no declaration, as the front end writes it for a fortified header.
    /// The static spelling of the same question is not known. This is what gcc 16 does at `-O2`.
    #[test]
    fn an_address_somewhere_in_one_object_is_measured_when_the_program_runs() {
        let body = "
global @g : bytes 32 = { zero 32 }, align 1, linkage(external)
func @strchr(ptr, i32) -> ptr, linkage(external);

func @f(ptr, i64), linkage(external) {
block0(%0: ptr, %1: i64):
    %2 = alloca, size 1000, align 16
    jump block1(%2)

block1(%3: ptr):
    %4 = object_size.i64 %3, kind 5
    call @use(%4) : (i64)
    %5 = object_size.i64 %3, kind 1
    call @use(%5) : (i64)
    %6 = iconst.i32 0
    %7 = call @strchr(%3, %6) : (ptr, i32) -> ptr
    %8 = iconst.i64 -1
    %9 = ptr_add %7, %8
    %10 = call @__strcpy_chk(%9, %0, %8) : (ptr, ptr, i64) -> ptr
    %11 = ptr_add %10, %1
    %12 = iconst.i64 0
    %13 = icmp ne %1, %12
    br_if %13, block1(%11), block2

block2:
    %14 = global_addr @g
    %15 = ptr_add %14, %1
    %16 = object_size.i64 %15, kind 4
    call @use(%16) : (i64)
    return
}
";
        assert_eq!(answers(body, true), [-1]);
        assert_eq!(answers(body, false), [-1, -1, -1]);
        let text = printed(body, Some(&[]));
        // The local's offset is the difference of two addresses, since the loop is in the way, and
        // the global's is the one step from it.
        assert_eq!(text.matches("ptrtoint").count(), 2, "{text}");
        assert_eq!(text.matches("icmp ugt").count(), 2, "{text}");
        assert!(text.contains("iconst.i64 1000") && text.contains("iconst.i64 32"), "{text}");
    }

    /// An address that may be in either of two objects, one a `strchr` gave back for a character
    /// that may not be there, and one in a global the linker may take from somewhere else are not
    /// measured. Neither is one a copy gave back under `-fno-builtin`, nor under
    /// `-fno-builtin-strcpy`, since the call may then be to anything.
    #[test]
    fn an_address_that_may_be_in_another_object_is_not_measured() {
        let body = "
global @w : bytes 8 = { zero 8 }, align 1, linkage(weak)
func @strchr(ptr, i32) -> ptr, linkage(external);
func @strcpy(ptr, ptr) -> ptr, linkage(external);

func @f(i1, i64, ptr), linkage(external) {
block0(%0: i1, %1: i64, %2: ptr):
    %3 = alloca, size 10, align 1
    %4 = alloca, size 20, align 1
    %5 = select.ptr %0, %3, %4
    %6 = ptr_add %5, %1
    %7 = object_size.i64 %6, kind 4
    call @use(%7) : (i64)
    %8 = iconst.i32 97
    %9 = call @strchr(%3, %8) : (ptr, i32) -> ptr
    %10 = ptr_add %9, %1
    %11 = object_size.i64 %10, kind 4
    call @use(%11) : (i64)
    %12 = global_addr @w
    %13 = ptr_add %12, %1
    %14 = object_size.i64 %13, kind 4
    call @use(%14) : (i64)
    %15 = call @strcpy(%3, %2) : (ptr, ptr) -> ptr
    %16 = ptr_add %15, %1
    %17 = object_size.i64 %16, kind 4
    call @use(%17) : (i64)
    return
}
";
        assert_eq!(answers(body, true), [-1, -1, -1]);
        assert_eq!(printed(body, Some(&[])).matches("ptrtoint").count(), 2);
        assert_eq!(printed(body, None).matches("ptrtoint").count(), 0);
        assert_eq!(printed(body, Some(&["strcpy".to_owned()])).matches("ptrtoint").count(), 0);
    }

    /// Each value is walked once. Here the address goes through seven joins with sixteen ways into
    /// each, so a walk that went once for each way to a value would take more than two hundred
    /// million steps. lua's interpreter loop has joins like these, and its build did not end.
    #[test]
    fn a_walk_through_many_joins_goes_to_each_value_once() {
        let mut body = String::from(
            "func @strchr(ptr, i32) -> ptr, linkage(external);\n\n\
             func @f(i1), linkage(external) {\n\
             block0(%0: i1):\n    %1 = alloca, size 100, align 16\n    %2 = iconst.i32 0\n    \
             jump block1(%1)\n",
        );
        let mut value = 3;
        let mut block = 1;
        for _ in 0..7 {
            let (address, found) = (value, value + 1);
            value += 2;
            let next = block + 31;
            body += &format!("\nblock{block}(%{address}: ptr):\n");
            body += &format!("    %{found} = call @strchr(%{address}, %2) : (ptr, i32) -> ptr\n");
            body += &format!("    br_if %0, block{}, block{}\n", block + 16, block + 1);
            for way in 1..15 {
                body += &format!(
                    "\nblock{}:\n    br_if %0, block{}, block{}\n",
                    block + way,
                    block + way + 16,
                    block + way + 1
                );
            }
            for way in 15..31 {
                body += &format!("\nblock{}:\n    jump block{next}(%{found})\n", block + way);
            }
            block = next;
        }
        body += &format!(
            "\nblock{block}(%{value}: ptr):\n    %{} = object_size.i64 %{value}, kind 5\n    \
             call @use(%{}) : (i64)\n    return\n}}\n",
            value + 1,
            value + 1
        );
        let text = printed(&body, Some(&[]));
        assert_eq!(text.matches("ptrtoint").count(), 2, "{text}");
    }

    /// An address past the end of its object has nothing left, and a global the linker may take
    /// from somewhere else is not one whose size this module knows.
    #[test]
    fn past_the_end_is_nothing_and_a_weak_global_is_not_known() {
        let body = "
global @g : bytes 8 = { zero 8 }, align 1, linkage(external)
global @w : bytes 8 = { zero 8 }, align 1, linkage(weak)

func @f(), linkage(external) {
block0:
    %0 = global_addr @g
    %1 = iconst.i64 12
    %2 = ptr_add %0, %1
    %3 = object_size.i64 %2, kind 0
    call @use(%3) : (i64)
    %4 = global_addr @w
    %5 = object_size.i64 %4, kind 0
    call @use(%5) : (i64)
    return
}
";
        assert_eq!(answers(body, true), [0, -1]);
    }

    /// An address loaded out of a read only global that holds it is the object it points at,
    /// which is `static const char *ptr_of_11 = "this is 11!"` in fortify_kunit. One that may be
    /// written, or holds an address into the middle of something, is not known.
    #[test]
    fn an_address_read_out_of_a_read_only_global_is_what_it_points_at() {
        let body = "
global @s : bytes 12 = { zero 12 }, align 1, linkage(internal), constant
global @p : bytes 8 = { addr.8 @s }, align 8, linkage(internal), constant
global @q : bytes 8 = { addr.8 @s }, align 8, linkage(internal)
global @r : bytes 8 = { addr.8 @s + 4 }, align 8, linkage(internal), constant

func @f(), linkage(external) {
block0:
    %0 = global_addr @p
    %1 = load.ptr %0, align 8
    %2 = object_size.i64 %1, kind 1
    call @use(%2) : (i64)
    %3 = global_addr @q
    %4 = load.ptr %3, align 8
    %5 = object_size.i64 %4, kind 1
    call @use(%5) : (i64)
    %6 = global_addr @r
    %7 = load.ptr %6, align 8
    %8 = object_size.i64 %7, kind 1
    call @use(%8) : (i64)
    return
}
";
        assert_eq!(answers(body, true), [12, -1, -1]);
    }

    /// An address lowering marked as the start of a member, or one a constant into it, has what
    /// is left of the member for the closest kinds and what is left of the whole object for the
    /// others. Without a mark on the way the smallest closest answer is not known, and the marks
    /// are gone once the questions are answered. The numbers are what gcc 16 answers at `-O2`.
    #[test]
    fn the_closest_member_is_the_one_lowering_marked() {
        let body = "
global @g : bytes 40 = { zero 40 }, align 4, linkage(external)

func @f(), linkage(external) {
block0:
    %0 = global_addr @g
    %1 = iconst.i64 4
    %2 = ptr_add %0, %1, member 16
    %3 = object_size.i64 %2, kind 1
    call @use(%3) : (i64)
    %4 = object_size.i64 %2, kind 3
    call @use(%4) : (i64)
    %5 = object_size.i64 %2, kind 0
    call @use(%5) : (i64)
    %6 = iconst.i64 2
    %7 = ptr_add %2, %6
    %8 = object_size.i64 %7, kind 1
    call @use(%8) : (i64)
    %9 = ptr_add %0, %1
    %10 = object_size.i64 %9, kind 1
    call @use(%10) : (i64)
    %11 = object_size.i64 %9, kind 3
    call @use(%11) : (i64)
    return
}
";
        assert_eq!(answers(body, true), [16, 16, 36, 14, 36, 0]);
        let mut names = Interner::new();
        let mut module = rucc_ir::parse(&format!("{HEAD}{body}"), &mut names).expect("parses");
        answer(&mut module, &names, Some(&[]), Pic::Executable, true);
        let text = rucc_ir::print(&module, &names);
        assert!(!text.contains("member"), "{text}");
    }

    /// A member of an object reached through a pointer has what is left of the member or what is
    /// left of the whole object, whichever is less, and the dynamic spelling works the whole
    /// object out when the program runs where it has to. A member of a local stepped into has the
    /// member alone. gcc 15 answers `c->name` with `c` from `malloc (10)` with 6, `b->name` with
    /// `b` from `malloc (n)` with the smaller of 16 and `n - 4`, and `p->mac` at a `p` a loop moves
    /// through a local with the smaller of 6 and what is left of the local.
    #[test]
    fn a_member_through_a_pointer_has_no_more_than_the_whole_object_has_left() {
        let body = "
func @my(i64) -> ptr, linkage(external), attrs(alloc_size=1);

func @f(i64, ptr), linkage(external) {
block0(%0: i64, %1: ptr):
    %2 = iconst.i64 10
    %3 = call @my(%2) : (i64) -> ptr
    %4 = iconst.i64 4
    %5 = ptr_add %3, %4, array 16
    %6 = object_size.i64 %5, kind 5
    call @use(%6) : (i64)
    %7 = object_size.i64 %5, kind 1
    call @use(%7) : (i64)
    %8 = ptr_add %1, %4, array 16
    %9 = object_size.i64 %8, kind 5
    call @use(%9) : (i64)
    %10 = alloca, size 24, align 8
    %11 = ptr_add %10, %4, array 16
    %12 = object_size.i64 %11, kind 5
    call @use(%12) : (i64)
    %13 = call @my(%0) : (i64) -> ptr
    %14 = ptr_add %13, %4, array 16
    %15 = object_size.i64 %14, kind 5
    call @use(%15) : (i64)
    %16 = object_size.i64 %14, kind 1
    call @use(%16) : (i64)
    return
}
";
        // The call given ten is read back too.
        assert_eq!(answers(body, true), [10, 6, 6, 16, 16, 16]);
        let text = printed(body, Some(&[]));
        assert_eq!(text.matches("icmp ugt").count(), 2, "{text}");
        assert_eq!(text.matches("select").count(), 2, "{text}");
        let looped = "
func @g(i32), linkage(external) {
block0(%0: i32):
    %1 = alloca, size 64, align 16
    %2 = iconst.i64 8
    %3 = ptr_add %1, %2, member 56
    %4 = iconst.i32 0
    jump block1(%3, %4)

block1(%5: ptr, %6: i32):
    %7 = iconst.i64 2
    %8 = ptr_add %5, %7, array 6
    %9 = object_size.i64 %8, kind 5
    call @use(%9) : (i64)
    %10 = object_size.i64 %8, kind 1
    call @use(%10) : (i64)
    %11 = iconst.i64 8
    %12 = ptr_add %5, %11
    %13 = iconst.i32 1
    %14 = add %6, %13
    %15 = icmp slt %14, %0
    br_if %15, block1(%12, %14), block2

block2:
    return
}
";
        assert_eq!(answers(looped, true), [6]);
        let text = printed(looped, Some(&[]));
        assert_eq!(text.matches("ptrtoint").count(), 2, "{text}");
        assert!(text.contains("iconst.i64 64"), "{text}");
        assert_eq!(text.matches("icmp ugt").count(), 2, "{text}");
    }

    /// A member of zero may run past its end, and it is asked about as the whole object whatever
    /// member the address it is in was worked out from. That is `&k->v` in bcachefs's
    /// `bkey_xattr_init`, where `k` is worked out from `&_k->k` and `v` ends in a flexible array.
    #[test]
    fn a_member_that_may_run_past_its_end_is_the_whole_object() {
        let body = "
global @g : bytes 40 = { zero 40 }, align 8, linkage(external)

func @f(), linkage(external) {
block0:
    %0 = global_addr @g
    %1 = iconst.i64 0
    %2 = ptr_add %0, %1, member 32
    %3 = iconst.i64 32
    %4 = ptr_add %2, %3, member 0
    %5 = object_size.i64 %4, kind 1
    call @use(%5) : (i64)
    %6 = object_size.i64 %4, kind 0
    call @use(%6) : (i64)
    %7 = object_size.i64 %4, kind 3
    call @use(%7) : (i64)
    %8 = iconst.i64 2
    %9 = ptr_add %4, %8
    %10 = object_size.i64 %9, kind 1
    call @use(%10) : (i64)
    %11 = iconst.i64 8
    %12 = ptr_add %2, %11, member 0
    %13 = object_size.i64 %12, kind 1
    call @use(%13) : (i64)
    return
}
";
        assert_eq!(answers(body, true), [8, 8, 8, 6, 32]);
    }

    /// A trailing array is as long as it says where the pointer is a constant step into a local
    /// or a global, and is the whole object behind the pointer anywhere else, whatever member the
    /// pointer was worked out from. That is gcc 16 at `-O2` on mwifiex's
    /// `mwifiex_cmd_802_11_key_material_v1`, which casts `&cmd->params.key_material` to another
    /// layout and clears the array at its end: 240 for a `cmd` on the stack, and not known for one
    /// passed in, where the member under it would have said 60.
    #[test]
    fn a_trailing_array_is_its_own_length_only_in_a_declared_object() {
        let body = "
global @g : bytes 304 = { zero 304 }, align 1, linkage(external)

func @f(ptr), linkage(external) {
block0(%0: ptr):
    %1 = global_addr @g
    %2 = iconst.i64 4
    %3 = ptr_add %1, %2, member 300
    %4 = iconst.i64 0
    %5 = ptr_add %3, %4, member 62
    %6 = iconst.i64 2
    %7 = ptr_add %5, %6, trailing 240
    %8 = object_size.i64 %7, kind 1
    call @use(%8) : (i64)
    %9 = object_size.i64 %7, kind 0
    call @use(%9) : (i64)
    %10 = ptr_add %0, %2, member 300
    %11 = ptr_add %10, %4, member 62
    %12 = ptr_add %11, %6, trailing 240
    %13 = object_size.i64 %12, kind 1
    call @use(%13) : (i64)
    %14 = object_size.i64 %12, kind 5
    call @use(%14) : (i64)
    %15 = iconst.i64 100
    %16 = ptr_add %3, %15, trailing 240
    %17 = object_size.i64 %16, kind 1
    call @use(%17) : (i64)
    return
}
";
        assert_eq!(answers(body, true), [240, 298, -1, -1, 200]);
        let text = printed(body, Some(&[]));
        assert!(!text.contains("trailing"), "{text}");
    }

    /// A `ptr_add` whose count the program worked out leaves the closest kinds with all of the
    /// member it moved in, or what is left of the whole object where that is less, and the flag is
    /// gone once the questions are answered. The numbers are what gcc 16 answers at `-O2` for
    /// `s.args + i` and `buf + i` with `i` a local holding one.
    #[test]
    fn a_counted_step_inside_a_member_is_somewhere_in_all_of_it() {
        let body = "
global @g : bytes 66 = { zero 66 }, align 2, linkage(external)
global @b : bytes 16 = { zero 16 }, align 1, linkage(external)

func @f(), linkage(external) {
block0:
    %0 = global_addr @g
    %1 = iconst.i64 2
    %2 = ptr_add %0, %1, member 62
    %3 = ptr_add.counted %2, %1
    %4 = object_size.i64 %3, kind 1
    call @use(%4) : (i64)
    %5 = object_size.i64 %3, kind 0
    call @use(%5) : (i64)
    %6 = ptr_add %2, %1
    %7 = object_size.i64 %6, kind 1
    call @use(%7) : (i64)
    %8 = global_addr @b
    %9 = iconst.i64 1
    %10 = ptr_add.counted %8, %9
    %11 = object_size.i64 %10, kind 1
    call @use(%11) : (i64)
    return
}
";
        assert_eq!(answers(body, true), [62, 62, 60, 15]);
        let mut names = Interner::new();
        let mut module = rucc_ir::parse(&format!("{HEAD}{body}"), &mut names).expect("parses");
        answer(&mut module, &names, Some(&[]), Pic::Executable, true);
        let text = rucc_ir::print(&module, &names);
        assert!(!text.contains("counted"), "{text}");
    }

    /// The dynamic spelling of the closest member, about an address a step the program works out
    /// past the start of a member, is the length of the member less how far in the address is,
    /// worked out when the program runs, wherever the structure is. That is
    /// `__builtin_dynamic_object_size (&d->flit[i], 1)` with `d` a parameter, which gcc 16 answers
    /// with `128 - 8 * i` and nothing past the end. The whole object stays not known, and so does
    /// a member that may run past its end.
    #[test]
    fn a_step_the_program_works_out_inside_a_member_is_measured_when_the_program_runs() {
        let body = "
func @f(ptr, i64), linkage(external) {
block0(%0: ptr, %1: i64):
    %2 = iconst.i64 0
    %3 = ptr_add %0, %2, array 128
    %4 = ptr_add.counted %3, %1
    %5 = object_size.i64 %4, kind 5
    call @use(%5) : (i64)
    %6 = object_size.i64 %4, kind 4
    call @use(%6) : (i64)
    %7 = object_size.i64 %4, kind 1
    call @use(%7) : (i64)
    %8 = iconst.i64 8
    %9 = ptr_add %0, %8, member 0
    %10 = ptr_add.counted %9, %1
    %11 = object_size.i64 %10, kind 5
    call @use(%11) : (i64)
    return
}
";
        assert_eq!(answers(body, true), [-1, -1, -1]);
        let text = printed(body, Some(&[]));
        assert_eq!(text.matches("ptrtoint").count(), 0, "{text}");
        let taken = text.lines().any(|line| line.contains(" = sub ") && line.ends_with(", %1"));
        assert!(taken, "the step is taken off the length, {text}");
        assert_eq!(text.matches("icmp ugt").count(), 1, "{text}");
        assert!(text.contains("iconst.i64 128"), "{text}");
    }

    /// Each way into a join may bring an answer of its own for the dynamic spelling, a member with
    /// a step the program works out on one and the start of the member or a local on the other,
    /// and the join is given a parameter each way passes its answer in, as gcc 16 does for arcmsr's
    /// `&acb->wqbuffer[acb->put]` or `acb->wqbuffer`. A step after the join is taken off that. A
    /// way that brings nothing known leaves the whole join not known, and so does the other
    /// spelling.
    #[test]
    fn each_way_into_a_join_brings_its_own_answer() {
        let body = |other: &str| {
            format!(
                "
func @f(ptr, i64, i64), linkage(external) {{
block0(%0: ptr, %1: i64, %2: i64):
    %3 = alloca, size 1, align 1
    %4 = iconst.i64 16
    %5 = ptr_add %0, %4, array 4096
    %6 = ptr_add.counted %5, %1
    %7 = iconst.i64 0
    %8 = icmp ne %2, %7
    br_if %8, block1, block2

block1:
    %9 = {other}
    jump block3(%9)

block2:
    jump block3(%6)

block3(%10: ptr):
    %11 = object_size.i64 %10, kind 5
    call @use(%11) : (i64)
    %12 = object_size.i64 %10, kind 1
    call @use(%12) : (i64)
    %13 = ptr_add %10, %1
    %14 = object_size.i64 %13, kind 5
    call @use(%14) : (i64)
    return
}}
"
            )
        };
        for (other, brought) in [("ptr_add %0, %4, array 4096", "4096"), ("ptr_add %3, %7", "1")] {
            let body = body(other);
            assert_eq!(answers(&body, true), [-1], "{other}");
            let text = printed(&body, Some(&[]));
            let joined =
                text.lines().any(|line| line.starts_with("block3(") && line.ends_with(": i64):"));
            assert!(joined, "the join is given a parameter, {text}");
            assert!(text.contains(&format!("iconst.i64 {brought}")), "{text}");
            // One for the way with a step on each question, and one for the step after the join.
            assert_eq!(text.matches("icmp ugt").count(), 3, "{text}");
        }
        assert_eq!(answers(&body("ptr_add %0, %1"), true), [-1, -1, -1]);
    }

    /// A way into a join that brings a null pointer brings nothing to a question only asked once a
    /// branch has found the pointer is not null, as in maple_tree's `node_copy`. Asked before the
    /// branch, the join is not known.
    #[test]
    fn a_null_pointer_a_branch_has_ruled_out_brings_nothing() {
        let body = |test: &str| {
            format!(
                "
func @f(ptr, i64, i1), linkage(external) {{
block0(%0: ptr, %1: i64, %2: i1):
    br_if %2, block1, block2

block1:
    %3 = iconst.i64 16
    %4 = ptr_add %0, %3, array 80
    jump block3(%4)

block2:
    %5 = iconst.i64 0
    %6 = inttoptr.ptr %5
    jump block3(%6)

block3(%7: ptr):
    %8 = iconst.i64 0
    %9 = inttoptr.ptr %8
    %10 = icmp {test} %7, %9
    %11 = ptr_add %7, %1
    %12 = object_size.i64 %11, kind 5
    call @use(%12) : (i64)
    br_if %10, block4, block5

block4:
    %13 = object_size.i64 %11, kind 5
    call @use(%13) : (i64)
    return

block5:
    return
}}
"
            )
        };
        let text = printed(&body("ne"), Some(&[]));
        assert_eq!(text.matches("iconst.i64 -1").count(), 1, "{text}");
        assert_eq!(text.matches("iconst.i64 80").count(), 1, "{text}");
        let text = printed(&body("eq"), Some(&[]));
        assert_eq!(text.matches("iconst.i64 -1").count(), 2, "{text}");
    }

    /// What `kcalloc (1, size, gfp)` is once its `kmalloc_array_noprof` is inlined: a product
    /// checked for overflow and put into `bytes`, a null pointer on the way it overflowed, and two
    /// calls that each read `bytes` back. A product with 1 in it cannot overflow, so the null
    /// pointer never comes back and the answer is the product, as fortify_kunit's
    /// `fortify_test_alloc_size_kmalloc_dynamic` expects. With 2 the null pointer may come back
    /// and the answer is not known.
    #[test]
    fn a_size_read_back_on_each_way_to_the_allocator_is_the_one_stored() {
        let body = |by: u32| {
            format!(
                "
func @my(i64) -> ptr, linkage(external), attrs(alloc_size=1);

func @f(i64, i32), linkage(external) {{
block0(%0: i64, %1: i32):
    %2 = alloca, size 8, align 8
    %3 = iconst.i64 {by}
    %4, %5 = umul_overflow.(i64, i1) %3, %0
    store %4 -> %2, align 8
    br_if %5, block1, block2

block1:
    %6 = iconst.i64 0
    %7 = inttoptr.ptr %6
    jump block5(%7)

block2:
    %8 = iconst.i32 0
    %9 = icmp ne %1, %8
    br_if %9, block3, block4

block3:
    %10 = load.i64 %2, align 8
    %11 = call @my(%10) : (i64) -> ptr
    jump block5(%11)

block4:
    %12 = load.i64 %2, align 8
    %13 = call @my(%12) : (i64) -> ptr
    jump block5(%13)

block5(%14: ptr):
    %15 = object_size.i64 %14, kind 4
    call @use(%15) : (i64)
    return
}}
"
            )
        };
        assert_eq!(answers(&body(1), true), [] as [i128; 0]);
        let text = printed(&body(1), Some(&[]));
        assert!(text.contains("call @use(%4)"), "{text}");
        assert_eq!(answers(&body(2), true), [-1]);
    }

    /// A flexible array member with `counted_by` is as long as its count for every kind of the
    /// dynamic spelling, read right after the step to the array and from the record itself, and
    /// less how far into the array the address is. The other spelling is left to the whole
    /// object, which a pointer says nothing about. gcc 16 gives the same at `-O2`.
    #[test]
    fn a_counted_flexible_array_is_as_long_as_its_count() {
        let body = |kind: u8| {
            format!(
                "
func @f(ptr, i64), linkage(external) {{
block0(%0: ptr, %1: i64):
    %2 = iconst.i64 4
    %3 = ptr_add %0, %2, counted i32 -4 2
    %4 = ptr_add.counted %3, %1
    %5 = object_size.i64 %4, kind {kind}
    call @use(%5) : (i64)
    return
}}
"
            )
        };
        let mut names = Interner::new();
        let module = rucc_ir::parse(&format!("{HEAD}{}", body(5)), &mut names).expect("parses");
        let text = rucc_ir::print(&module, &names);
        assert!(text.contains("ptr_add %0, %2, counted i32 -4 2"), "{text}");
        for kind in 4..8 {
            let text = printed(&body(kind), Some(&[]));
            assert!(text.contains("%3 = ptr_add %0, %2\n"), "{text}");
            assert!(text.contains("= load.i32 %0"), "{text}");
            assert!(text.contains("icmp slt"), "{text}");
            assert!(text.contains("= mul"), "{text}");
            assert!(!text.contains("counted"), "{text}");
        }
        assert_eq!(answers(&body(1), true), [-1]);
        assert_eq!(answers(&body(2), true), [0]);
    }
}
