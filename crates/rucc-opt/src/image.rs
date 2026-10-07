//! What a load from an object nothing can write to reads, which is what the object was
//! initialized to.
//!
//! A `const` object with static storage duration and an initializer is bytes the program cannot
//! change, so a load from one at an offset the compiler knows has an answer before the program
//! runs. Working it out is the oldest optimization there is and rucc did not have it:
//! `crate::load` forwards a load from a store earlier in the same block, and nothing anywhere
//! looked at what a global was initialized to, so a `const` table read at a constant index kept
//! its load and kept the arithmetic around it. That is issue 1358.
//!
//! # Where the image comes from
//!
//! A global's image lives on the module and a pass is handed one function, which is the problem
//! `crate::extents` has and solves by running once over the module before the pipeline starts.
//! That way out is wrong here, and finding out why is most of what this file is.
//!
//! What the frontend hands over for `t[2]` is not an offset. It is the index sign extended, a
//! multiply by the element size, and a `ptr_add` of the product, because the lowering walk writes
//! a subscript the way C defines one and leaves the arithmetic to the pipeline. So a step that ran
//! before the pipeline would read an address it could not evaluate and fold nothing at all, on
//! every array and every string in the program. `crate::extents` accepts exactly this cost for the
//! same reason, and can, because what it loses is a bounds check it did not discharge. What is
//! lost here is the whole optimization.
//!
//! So this is a pass, and the module's images reach it the way the machine does, on
//! [`crate::Analyses`]. `crate::machine` argues that at length and the argument is the same one:
//! the analysis cache is the one thing every pass is handed besides the function and its fuel, so
//! a fact about the module that a pass needs goes there rather than onto a fourth parameter of
//! every `run`. The pipeline builds one [`Images`] for the module and each function's cache holds
//! a counted reference to it.
//!
//! The difference from the machine is that this is a table rather than two words, so the pipeline
//! builds it only when a level runs this pass, and what it copies out of the module is the image
//! of the globals that are read only and nothing else. A `const` table of a megabyte is copied
//! once per compilation, which is the price of a pass being handed a function.
//!
//! # Where it runs
//!
//! With a `fold` on each side of it, at every level that optimizes and at none that does not.
//!
//! The one ahead is what turns the subscript arithmetic above into the offset this reads, so
//! without it this answers nothing. The one behind is the mirror of that, and it is the half that
//! is easy to leave out. What this writes is a constant where a load stood, and standing on top of
//! it is whatever the program did with the value: `(int) one != 1` on a `const double` is a
//! conversion and a comparison, and folding those is what turns the branch into a branch the
//! control flow passes can take out. Nothing later in the list arrives in time, because the branch
//! passes read the condition and a condition still spelled as a conversion of a constant is a
//! branch they leave standing. That is the difference between a program that links and one that
//! does not, which is what `gcc.c-torture/execute/20030216-1.c` is.
//!
//! # Which globals are believed
//!
//! The four conditions `crate::extents` sets, which is that module's `vouched`, and one more.
//! The extra one is that writing through a pointer to the object is undefined, which is
//! `Global::constant` and is what puts it in `.rodata`. A global that is not read only can be
//! written by anything holding its address, including code in another translation unit, and this
//! is not looking for the writes.
//!
//! Nothing here asks whether the object is reachable or whether its address is taken, because the
//! question is what the bytes are rather than who else can see them. An exported `const` table
//! folds for its own module and stays in the output for everybody else's.
//!
//! # What the image answers
//!
//! A run of zero bytes answers zero. A scalar answers the value it holds, when the access starts
//! where the scalar does and is exactly as wide, because that is the case where no question of
//! byte order arises: the image keeps a scalar as a value, and which byte of it comes first is
//! decided when the object file is written rather than here. Literal bytes answer the number they
//! spell in the order the datalayout puts them, which is where the byte order does have to be
//! asked about and is the only place it is.
//!
//! The address of another symbol answers that symbol's address, for a load of a pointer that reads
//! exactly the relocation and no more, and only where the address is the symbol itself rather than
//! a distance past it. The value is not known until the link, but a `global_addr` of the same name
//! is the same value then, and that is all the load needs. A table of operations is what this is
//! for: `split_ops.add (vq, ...)` in drivers/virtio/virtio_ring.c reads a member of a `static
//! const` structure, and gcc calls `virtqueue_add_split` there directly and then inlines it.
//!
//! A name answered this way is often tested against null straight after, since the kernel checks
//! its tables with `BUILD_BUG_ON (!table[i].member)`: madera's mixer names and rtw89's SAR
//! handlers are both read like that. The test is a call to a function declared `error` that has to
//! be gone by the end, and `crate::fold::addresses` decided every such test before the pipeline
//! began, when the load was still a load. So the pass decides it here too, for a name the module
//! gives a body, which is the rule that function keeps and gcc's under
//! `-fno-delete-null-pointer-checks`.
//!
//! # A call through an address that is a name
//!
//! A call through a pointer that turns out to be the address of a function is a call to that
//! function. This is where that is decided too, since a call through a member of a table only
//! becomes one once the load above has been answered. The call is only rewritten when the
//! function's own signature is the one the call was made with, so a call through a pointer cast
//! to some other type stays what it was. A call made directly is one the inliner can copy, and in
//! the kernel it is a `call` rather than a call to a retpoline thunk.
//!
//! Neither does an access that crosses from one piece of the image into the next, since
//! bytes spanning two of them are not a scalar either one holds, and neither does a `volatile`
//! access or an atomic one, whose whole point is that the access happens.
//!
//! # A copy out of the image
//!
//! A `memcpy` of a known size out of one of these objects becomes stores of what the image holds
//! there, with one fill in front when the object has more than a few zero bytes in that range.
//! Every byte has to be answered for this to happen, so a piece of a scalar or an address with
//! something added to it leaves the copy as it is. `spelled` says why the kernel needs it.

use std::collections::hash_map::Entry;

use rucc_base::Symbol;
use rucc_base::hash::{Map, Set};
use rucc_ir::{
    Block, Datum, Def, Extra, Flags, Func, Imm, Inst, InstData, MemInfo, MemOrder, Module, Opcode,
    Pic, Restrict, Signature, SymbolRef, Type, Value,
};

use crate::extents::vouched;
use crate::{Analyses, Analysis, Fuel, Pass, Preserved, Stats};

/// Recorded once for each load that became a constant.
const FOLDED: &str = "load from a read only object folded to what it was initialized to";

/// Recorded once for each load of a pointer that became the name it was initialized to.
const NAMED: &str = "load from a read only object folded to the address it was initialized to";

/// Recorded once for each test of a name against null that became the answer.
const NOT_NULL: &str = "test of an address with a body against null folded";

/// Recorded once for each call through a pointer that became a call by name.
const DIRECT: &str = "call through the address of a function made a direct call";

/// Recorded once for each copy out of a read only object that became stores of what it holds.
const COPIED: &str = "copy out of a read only object made stores of what it was initialized to";

/// The most bytes a copy out of a read only object is spelled out for.
const LONGEST: u64 = 512;

/// The most stores a copy is spelled out as, not counting the fill that clears it first.
const MOST: usize = 24;

/// How many zero bytes an object has to hold before they are cleared with one fill of the whole
/// destination rather than written by stores of their own. Sixteen is two eight byte stores,
/// which is about what the fill costs.
const FILL_OVER: u64 = 16;

/// Recorded for a load that would have folded if there had been fuel for it.
const NO_FUEL: &str = "load from a read only object not folded, the pass ran out of fuel";

/// What the pass is called, which [`crate::pipeline`] needs before it has run anything, to decide
/// whether building the table is worth it.
pub const NAME: &str = "image";

/// The pass. It holds nothing, because the images are on the analysis cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Image;

impl Pass for Image {
    fn name(&self) -> &'static str {
        NAME
    }

    fn describe(&self) -> &'static str {
        "a load from an object nothing can write to becomes what the object was initialized to"
    }

    fn preserves(&self) -> Preserved {
        // A load becomes a constant where it stands, so no block moves and no edge moves. Not the
        // liveness, for the reason `crate::fold` gives: the address the load was reading is read
        // by nobody now.
        Preserved::ALL.without(Analysis::Liveness)
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        let images = an.images();
        if images.is_empty() {
            return stats;
        }
        settle(func, images, fuel, &mut stats);
        stats
    }
}

/// Answers every load in the function the images can answer, and then makes every call through
/// the address of a function a call to it.
///
/// The pass, and also what [`crate::inline`] runs on each function before it looks at its calls,
/// since gcc's early passes have folded these by the time it decides what to inline.
pub(crate) fn settle(func: &mut Func, images: &Images, fuel: &mut Fuel, stats: &mut Stats) {
    let blocks: Vec<Block> = func.blocks().collect();
    for &block in &blocks {
        let insts: Vec<Inst> = func.insts(block).collect();
        for inst in insts {
            let Some(found) = answer(func, inst, images) else { continue };
            if !fuel.take() {
                // Out of fuel, which is a request to stop transforming rather than to stop
                // looking, per `crate::fold`.
                stats.missed(NO_FUEL);
                continue;
            }
            let extra = match found {
                Found::Constant(opcode, imm) => {
                    stats.optimized(FOLDED);
                    let at = func.add_imm(imm);
                    func[inst].opcode = opcode;
                    Extra::Imm(at)
                }
                Found::Address(name) => {
                    stats.optimized(NAMED);
                    func[inst].opcode = Opcode::GlobalAddr;
                    Extra::Symbol(name)
                }
                Found::Number(name) => {
                    stats.optimized(NAMED);
                    let data = InstData {
                        extra: Extra::Symbol(name),
                        ..InstData::new(Opcode::GlobalAddr)
                    };
                    let span = func.span(inst);
                    let made = func.create_inst(data, &[Type::PTR], span);
                    func.insert_before(made, inst);
                    let Some(address) = func[made].results().next() else { continue };
                    let args = func.push_values(&[address]);
                    let data = &mut func[inst];
                    data.opcode = Opcode::PtrToInt;
                    data.flags = Flags::NONE;
                    data.extra = Extra::None;
                    data.args = args;
                    continue;
                }
            };
            let data = &mut func[inst];
            data.flags = Flags::NONE;
            data.args = rucc_ir::ValueList::EMPTY;
            data.extra = extra;
        }
    }
    for &block in &blocks {
        let insts: Vec<Inst> = func.insts(block).collect();
        for inst in insts {
            let bodies = &images.bodies;
            let Some(answer) = crate::fold::against_null(func, inst, |name| bodies.contains(&name))
            else {
                continue;
            };
            if !fuel.take() {
                stats.missed(NO_FUEL);
                continue;
            }
            crate::fold::write(func, inst, answer);
            stats.optimized(NOT_NULL);
        }
    }
    for &block in &blocks {
        let insts: Vec<Inst> = func.insts(block).collect();
        for inst in insts {
            let Some(writes) = spelled(func, inst, images) else { continue };
            if !fuel.take() {
                stats.missed(NO_FUEL);
                continue;
            }
            unroll(func, inst, &writes, images.pointer);
            stats.optimized(COPIED);
        }
    }
    for &block in &blocks {
        let insts: Vec<Inst> = func.insts(block).collect();
        for inst in insts {
            let Some(name) = direct(func, inst, images) else { continue };
            if !fuel.take() {
                stats.missed(NO_FUEL);
                continue;
            }
            let Extra::Call(info) = func[inst].extra else { continue };
            let args = func[func[inst].args][1..].to_vec();
            let args = func.push_values(&args);
            let mut call = func[info];
            call.callee = Some(name);
            let at = func.add_call(call);
            let data = &mut func[inst];
            if data.opcode == Opcode::CallIndirect {
                data.opcode = Opcode::Call;
            }
            data.args = args;
            data.extra = Extra::Call(at);
            stats.optimized(DIRECT);
        }
    }
}

/// One store a copy out of a read only object comes to, at that many bytes past where it goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Write {
    /// The object is zero there for that many bytes, which the fill in front writes.
    Zero(u64),
    /// A number of that type, written with that opcode.
    Number(u64, Type, Opcode, Imm),
    /// The address of that symbol.
    Name(u64, Symbol),
}

/// What a copy out of a read only object writes, when the image answers every byte of it.
///
/// `hrtimer_hw` in sound/core/hrtimer.c and `iommu_pmu` in arch/x86/events/amd/iommu.c are
/// `__initconst` structures copied whole into an object the driver allocates, and the copy is all
/// that reads them. gcc folds the copy into the constructor the object was initialized with and
/// stores that, which leaves nothing reading the object and drops it with its `.init.rodata`.
/// rucc copied the bytes out of it, so the object stayed.
///
/// Every byte has to be answered. A number that is not where a piece of the image starts, or not
/// as wide as one, is a piece of a scalar and leaves the copy alone, and so does a relocation
/// with something added to it.
fn spelled(func: &Func, inst: Inst, images: &Images) -> Option<Vec<Write>> {
    let data = &func[inst];
    if !matches!(data.opcode, Opcode::Memcpy | Opcode::Memmove)
        || data.flags.intersects(Flags::KEEP)
        || func.carries_mem(inst)
    {
        return None;
    }
    let Extra::Mem(info) = data.extra else { return None };
    let bulk = func.bulk(inst)?;
    if bulk.length.is_some() || func[info].order != MemOrder::NotAtomic {
        return None;
    }
    let size = func[info].size;
    if size == 0 || size > LONGEST {
        return None;
    }
    let (base, offset) = address(func, bulk.with)?;
    let Def::Result { inst: made, .. } = func[base].def else { return None };
    if func[made].opcode != Opcode::GlobalAddr {
        return None;
    }
    let Extra::Symbol(name) = func[made].extra else { return None };
    let offset = u64::try_from(offset).ok()?;
    let writes = images.spell(name, offset, size)?;
    let stores = writes.iter().filter(|write| !matches!(write, Write::Zero(_))).count();
    (stores <= MOST).then_some(writes)
}

/// Puts the stores in place of the copy, behind one fill of zero when the object has a run of
/// zero bytes worth one. `word` is how many bytes an address takes, which is the widest store
/// and the width of the distance each one is moved by.
fn unroll(func: &mut Func, inst: Inst, writes: &[Write], word: u64) {
    let bulk = func.bulk(inst).expect("only a copy is spelled out");
    let Extra::Mem(info) = func[inst].extra else { unreachable!("a copy carries its payload") };
    let (size, align) = (func[info].size, func[info].align);
    let to = bulk.to;
    let zero: u64 =
        writes.iter().map(|write| if let Write::Zero(bytes) = write { *bytes } else { 0 }).sum();
    let filled = zero > FILL_OVER;
    if filled {
        let byte = constant(func, inst, Type::int(8), Opcode::IConst, Imm::int(0, Type::int(8)));
        let mem = func.add_mem(MemInfo { size, align, ..plain(align) });
        let args = func.push_values(&[to, byte]);
        let fill = InstData { args, extra: Extra::Mem(mem), ..InstData::new(Opcode::Memset) };
        let span = func.span(inst);
        let fill = func.create_inst(fill, &[], span);
        func.insert_before(fill, inst);
    }
    let mut at = 0u64;
    for &write in writes {
        let (value, width) = match write {
            Write::Zero(bytes) => {
                if !filled {
                    zeros(func, inst, to, at, bytes, align, word);
                }
                at += bytes;
                continue;
            }
            Write::Number(width, ty, opcode, imm) => (constant(func, inst, ty, opcode, imm), width),
            Write::Name(width, name) => {
                let data =
                    InstData { extra: Extra::Symbol(name), ..InstData::new(Opcode::GlobalAddr) };
                (emit(func, inst, data, Type::PTR), width)
            }
        };
        put(func, inst, value, to, at, width, align, word);
        at += width;
    }
    func.remove_inst(inst);
}

/// Stores of zero over that many bytes, a word at a time while there is a word.
fn zeros(func: &mut Func, before: Inst, to: Value, mut at: u64, bytes: u64, align: u32, word: u64) {
    let end = at + bytes;
    while at < end {
        let width = widest(word, end - at);
        let ty = Type::int(u32::try_from(width * 8).unwrap_or(8));
        let value = constant(func, before, ty, Opcode::IConst, Imm::int(0, ty));
        put(func, before, value, to, at, width, align, word);
        at += width;
    }
}

/// A plain store of that value, `at` bytes past `to`.
///
/// The distance is an integer as wide as an address, since a 64-bit one is a value i386 has no
/// instruction to add to an address.
#[allow(clippy::too_many_arguments)]
fn put(
    func: &mut Func,
    before: Inst,
    value: Value,
    to: Value,
    at: u64,
    width: u64,
    align: u32,
    word: u64,
) {
    let address = if at == 0 {
        to
    } else {
        let step = i128::from(at);
        let ty = Type::int(u32::try_from(word * 8).unwrap_or(64));
        let step = constant(func, before, ty, Opcode::IConst, Imm::int(step, ty));
        let args = func.push_values(&[to, step]);
        emit(func, before, InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR)
    };
    // What the copy promised for its start, for as far as it reaches this far in.
    let known =
        if at == 0 { u64::from(align) } else { u64::from(align).min(1 << at.trailing_zeros()) };
    let align = u32::try_from(known.min(width)).unwrap_or(1);
    let mem = func.add_mem(plain(align));
    let args = func.push_values(&[value, address]);
    let data = InstData { args, extra: Extra::Mem(mem), ..InstData::new(Opcode::Store) };
    let span = func.span(before);
    let store = func.create_inst(data, &[], span);
    func.insert_before(store, before);
}

/// A constant of that type, put in front of `before`.
fn constant(func: &mut Func, before: Inst, ty: Type, opcode: Opcode, imm: Imm) -> Value {
    let at = func.add_imm(imm);
    emit(func, before, InstData { extra: Extra::Imm(at), ..InstData::new(opcode) }, ty)
}

/// The widest store, no wider than a word, that fits in what is left.
fn widest(word: u64, left: u64) -> u64 {
    [8, 4, 2, 1].into_iter().find(|&width| width <= word && width <= left).unwrap_or(1)
}

/// Puts an instruction in front of another one and gives back the value it produces.
fn emit(func: &mut Func, before: Inst, data: InstData, ty: Type) -> Value {
    let span = func.span(before);
    let inst = func.create_inst(data, &[ty], span);
    func.insert_before(inst, before);
    func[inst].first_result.expect("one result was asked for")
}

/// What an ordinary access with nothing known about it carries.
const fn plain(align: u32) -> MemInfo {
    MemInfo {
        size: 0,
        align,
        order: MemOrder::NotAtomic,
        tbaa: None,
        owns: 0,
        restrict: Restrict::NONE,
    }
}

/// The function a call through a pointer reaches, when the pointer is its address and the call
/// was made with its signature.
fn direct(func: &Func, inst: Inst, images: &Images) -> Option<Symbol> {
    let data = &func[inst];
    let Extra::Call(info) = data.extra else { return None };
    let through = data.opcode == Opcode::CallIndirect
        || (data.opcode == Opcode::TailCall && func[info].callee.is_none());
    if !through {
        return None;
    }
    let Def::Result { inst: made, .. } = func[*func[data.args].first()?].def else { return None };
    if func[made].opcode != Opcode::GlobalAddr {
        return None;
    }
    let Extra::Symbol(name) = func[made].extra else { return None };
    (images.signatures.get(&name)? == &func[func[info].signature]).then_some(name)
}

/// What a load reads, as an instruction that can stand where it is.
enum Found {
    /// A number, written with this opcode.
    Constant(Opcode, Imm),
    /// The address of this symbol.
    Address(Symbol),
    /// The address of this symbol, read as an integer as wide as a pointer.
    Number(Symbol),
}

/// The initial image of every global in a module that a load can be answered out of.
///
/// Empty is the honest answer for a module with no such global and is also what a cache built
/// without one holds, which is why there is a `Default` here and none on [`crate::Machine`]. A
/// missing cost table is a pass optimizing for a machine nobody chose; a missing image is a load
/// that does not fold.
#[derive(Debug, Default, Clone)]
pub struct Images {
    /// By the name the global is reached by, and `None` for a name that arrived twice.
    objects: Map<Symbol, Option<Object>>,
    /// Which end of a number the target puts first, which only the literal bytes need.
    little_endian: bool,
    /// How many bytes a pointer is, which a `ptr` does not say itself.
    pointer: u64,
    /// The signature of every function the module defines or declares, which is what a call
    /// through the address of one has to have been made with to become a call to it.
    signatures: Map<Symbol, Signature>,
    /// Every name the module gives a body, a function's or an object's, whose address is never
    /// null.
    bodies: Set<Symbol>,
}

impl Images {
    /// The images of every read only global this module can vouch for.
    #[must_use]
    pub fn of(module: &Module, pic: Pic) -> Self {
        let mut objects: Map<Symbol, Option<Object>> = Map::default();
        for id in module.globals() {
            let global = &module[id];
            if !global.constant || !vouched(global, pic) {
                continue;
            }
            let object = Object::of(module, global);
            // A name that somehow arrives twice keeps neither image. That cannot happen in a
            // module the frontend built, and written this way the failure if it ever does is a
            // load that did not fold rather than a load folded out of the wrong object.
            match objects.entry(global.name) {
                Entry::Occupied(mut at) => *at.get_mut() = None,
                Entry::Vacant(at) => {
                    at.insert(Some(object));
                }
            }
        }
        let signatures = module
            .funcs()
            .map(|id| (module[id].name, module[id].signature().clone()))
            .filter(|&(name, _)| matches!(module.lookup(name), Some(SymbolRef::Func(_))))
            .collect();
        let bodies = module
            .funcs()
            .map(|id| module[id].name)
            .chain(module.globals().map(|id| module[id].name))
            .filter(|&name| crate::fold::defined(module, name))
            .collect();
        Self {
            objects,
            little_endian: module.datalayout.little_endian,
            pointer: u64::from(module.datalayout.pointer_bits.div_ceil(8)),
            signatures,
            bodies,
        }
    }

    /// Whether that name is read only data this module defines and nothing else can replace.
    ///
    /// Which is also a name whose distance from anything else in this file is a number once the
    /// program is linked, and that is what `crate::switch_conv` asks this for.
    #[must_use]
    pub fn holds(&self, name: Symbol) -> bool {
        self.objects.contains_key(&name)
    }

    /// Whether there is anything here to answer a load with.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.objects.is_empty() && self.signatures.is_empty()
    }

    /// The value of type `ty` that lies `offset` bytes into the image of that global.
    #[must_use]
    pub fn read(&self, name: Symbol, ty: Type, offset: u64) -> Option<Imm> {
        let size = u64::from(ty.bits().div_ceil(8));
        let (piece, into) = self.piece(name, offset, size)?;
        piece.read(ty, into, size, self.little_endian)
    }

    /// The symbol whose address lies `offset` bytes into the image of that global, for a read of
    /// a pointer that is exactly the address.
    #[must_use]
    pub fn address(&self, name: Symbol, offset: u64) -> Option<Symbol> {
        match self.piece(name, offset, self.pointer)? {
            (&Piece::Address { symbol, size }, 0) if size == self.pointer => Some(symbol),
            _ => None,
        }
    }

    /// What `size` bytes at `offset` into the image of that global are, as the stores that would
    /// write them, or nothing when some of them are bytes this cannot say.
    fn spell(&self, name: Symbol, offset: u64, size: u64) -> Option<Vec<Write>> {
        let object = self.objects.get(&name)?.as_ref()?;
        let end = offset.checked_add(size)?;
        if end > object.size {
            return None;
        }
        let mut writes = Vec::new();
        let mut at = 0u64;
        for piece in &object.pieces {
            let width = piece.size();
            let (from, to) = (at.max(offset), (at + width).min(end));
            let whole = from == at && to == at + width;
            at += width;
            if from >= to {
                continue;
            }
            match piece {
                Piece::Zero(_) => match writes.last_mut() {
                    Some(Write::Zero(bytes)) => *bytes += to - from,
                    _ => writes.push(Write::Zero(to - from)),
                },
                Piece::Scalar { ty, value } => {
                    // An integer wider than an address is one i386 has no store for.
                    let fits = whole && ty.is_scalar() && matches!(ty.bits(), 8 | 16 | 32 | 64);
                    if ty.is_int() && u64::from(ty.bits()) > self.pointer * 8 {
                        return None;
                    }
                    if !fits || !(ty.is_int() || ty.is_float()) {
                        return None;
                    }
                    let opcode = if ty.is_int() { Opcode::IConst } else { Opcode::FConst };
                    writes.push(Write::Number(width, *ty, opcode, *value));
                }
                Piece::Bytes(bytes) => {
                    let mut from = from;
                    while from < to {
                        let step = widest(self.pointer, to - from);
                        let into = usize::try_from(from - (at - width)).ok()?;
                        let run = &bytes[into..into + usize::try_from(step).ok()?];
                        let ty = Type::int(u32::try_from(step * 8).ok()?);
                        let value = Imm::int(assemble(run, self.little_endian) as i128, ty);
                        writes.push(Write::Number(step, ty, Opcode::IConst, value));
                        from += step;
                    }
                }
                Piece::Address { symbol, size } if whole && *size == self.pointer => {
                    writes.push(Write::Name(*size, *symbol));
                }
                Piece::Address { .. } | Piece::Opaque(_) => return None,
            }
        }
        Some(writes)
    }

    /// The piece of that global an access of `size` bytes at `offset` reads, and how far into it
    /// the access starts.
    fn piece(&self, name: Symbol, offset: u64, size: u64) -> Option<(&Piece, u64)> {
        let object = self.objects.get(&name)?.as_ref()?;
        let end = offset.checked_add(size)?;
        if size == 0 || end > object.size {
            return None;
        }
        let mut at = 0u64;
        for piece in &object.pieces {
            let width = piece.size();
            if at + width > offset {
                // The piece the access starts in, and the only one it may read, so an access
                // reaching past the end of this one is an access this cannot answer.
                return (at + width >= end).then_some((piece, offset - at));
            }
            at += width;
        }
        None
    }
}

/// One global's image, in the pieces the module wrote it in.
#[derive(Debug, Clone)]
struct Object {
    /// How many bytes the object is, which the pieces add up to.
    size: u64,
    /// What is in them, in order.
    pieces: Vec<Piece>,
}

impl Object {
    /// The image of a global this module defines.
    fn of(module: &Module, global: &rucc_ir::Global) -> Self {
        let data = global.init.map(|list| &module[list]).unwrap_or_default();
        let pieces = data
            .iter()
            .map(|&datum| match datum {
                Datum::Zero(bytes) => Piece::Zero(bytes),
                Datum::Bytes(range) => Piece::Bytes(module[range].to_vec()),
                Datum::Scalar { ty, value } => Piece::Scalar { ty, value: module[value] },
                // The address of a name this module knows, with nothing added to it.
                Datum::Addr(reloc)
                    if module[reloc].addend == 0
                        && module.lookup(module[reloc].symbol).is_some() =>
                {
                    Piece::Address { symbol: module[reloc].symbol, size: datum.size(module) }
                }
                // Kept rather than dropped, so that what follows it is still at the offset it is
                // at. What it holds is an address the linker has not written yet.
                Datum::Addr(_) | Datum::Away(_) | Datum::Apart { .. } => {
                    Piece::Opaque(datum.size(module))
                }
            })
            .collect();
        Self { size: global.size, pieces }
    }
}

/// One piece of an image, which is a [`Datum`] with what it refers to copied out of the module.
#[derive(Debug, Clone)]
enum Piece {
    /// That many zero bytes.
    Zero(u64),
    /// Those literal bytes.
    Bytes(Vec<u8>),
    /// One scalar of that type holding that value.
    Scalar { ty: Type, value: Imm },
    /// The address of that symbol, in that many bytes.
    Address { symbol: Symbol, size: u64 },
    /// That many bytes whose value this cannot say.
    Opaque(u64),
}

impl Piece {
    /// How many bytes of the image it is.
    fn size(&self) -> u64 {
        match self {
            Self::Zero(bytes) | Self::Opaque(bytes) | Self::Address { size: bytes, .. } => *bytes,
            Self::Bytes(bytes) => bytes.len() as u64,
            Self::Scalar { ty, .. } => u64::from(ty.bits().div_ceil(8)) * u64::from(ty.lanes()),
        }
    }

    /// The value an access of `size` bytes `into` this piece reads, as a constant of type `ty`.
    fn read(&self, ty: Type, into: u64, size: u64, little_endian: bool) -> Option<Imm> {
        match self {
            Self::Zero(_) => Some(number(ty, 0)),
            // Exactly this scalar and no part of it, which is the case byte order has no say in.
            // The image holds a scalar as a value rather than as bytes, so what comes back is what
            // was written, and an access of the same width starting where it starts reads it
            // whichever end of it the target puts first.
            Self::Scalar { ty: held, value } => {
                (into == 0 && held.is_scalar() && u64::from(held.bits().div_ceil(8)) == size)
                    .then_some(*value)
            }
            Self::Bytes(bytes) => {
                let into = usize::try_from(into).ok()?;
                let size = usize::try_from(size).ok()?;
                let bytes = bytes.get(into..into.checked_add(size)?)?;
                Some(number(ty, assemble(bytes, little_endian)))
            }
            // A relocation is a promise the linker has not kept yet, so there is no number here to
            // read at all. This is where `&other` written into an initializer stops.
            Self::Address { .. } | Self::Opaque(_) => None,
        }
    }
}

/// What this instruction reads, if it is a load the images can answer.
///
/// The opcode comes back with the value because a constant of an integer type and a constant of a
/// floating point type are two different instructions, and which one to write is decided by the
/// type of the load rather than by what the image turned out to hold.
fn answer(func: &Func, inst: Inst, images: &Images) -> Option<Found> {
    let data = &func[inst];
    if data.opcode != Opcode::Load || data.results != 1 || data.flags.intersects(Flags::KEEP) {
        return None;
    }
    let Extra::Mem(info) = data.extra else { return None };
    if func[info].order != MemOrder::NotAtomic {
        return None;
    }
    let ty = func[data.results().next()?].ty;
    // A vector constant is a `splat` rather than an `iconst`, which is the reason `crate::fold`
    // gives for leaving one alone, and there is a second reason on top of it here: the image would
    // have to be read a lane at a time and every lane would have to agree.
    if !ty.is_scalar() || !(ty.is_int() || ty.is_float() || ty.is_ptr()) {
        return None;
    }
    let (base, offset) = address(func, *func[data.args].first()?)?;
    let Def::Result { inst: made, .. } = func[base].def else { return None };
    if func[made].opcode != Opcode::GlobalAddr {
        return None;
    }
    let Extra::Symbol(name) = func[made].extra else { return None };
    let offset = u64::try_from(offset).ok()?;
    if ty.is_ptr() {
        return images.address(name, offset).map(Found::Address);
    }
    // A structure of pointers passed by value is read a word at a time as integers, since that is
    // the class the ABI gives it, so a word holding an address is answered with the address made
    // a number. `hashtab_insert` is handed `symtab_key_params` that way in
    // security/selinux/ss/symtab.c, and gcc passes the two function addresses as immediates.
    if ty.is_int() && u64::from(ty.bits()) == images.pointer * 8 {
        if let Some(symbol) = images.address(name, offset) {
            return Some(Found::Number(symbol));
        }
    }
    let imm = images.read(name, ty, offset)?;
    Some(Found::Constant(if ty.is_int() { Opcode::IConst } else { Opcode::FConst }, imm))
}

/// The address this value is, as something it was computed from and a distance in bytes from it.
///
/// A `ptr_add` of a constant, as many times over as there are of them, because an index into an
/// array of structures is one of these per level and the frontend writes them one at a time.
/// Anything else ends the walk and is what comes back, which for a load from a global is the
/// `global_addr` and for every other load is something the caller will not recognise.
fn address(func: &Func, mut value: Value) -> Option<(Value, i128)> {
    let mut offset: i128 = 0;
    loop {
        let Def::Result { inst, .. } = func[value].def else { return Some((value, offset)) };
        if func[inst].opcode != Opcode::PtrAdd {
            return Some((value, offset));
        }
        let args = &func[func[inst].args];
        let (step, step_ty) = crate::fold::constant(func, *args.get(1)?)?;
        offset = offset.checked_add(step.signed(step_ty))?;
        value = *args.first()?;
    }
}

/// Those bytes as one number, in the order the target reads them in.
fn assemble(bytes: &[u8], little_endian: bool) -> u128 {
    let mut value = 0u128;
    // Most significant byte first, which is the last of them on a little endian target and the
    // first of them on a big endian one.
    if little_endian {
        for &byte in bytes.iter().rev() {
            value = value << 8 | u128::from(byte);
        }
    } else {
        for &byte in bytes {
            value = value << 8 | u128::from(byte);
        }
    }
    value
}

/// Those bits as a constant of that type, which is a value for an integer and a bit pattern for a
/// floating point number.
fn number(ty: Type, bits: u128) -> Imm {
    if ty.is_int() { Imm::int(bits as i128, ty) } else { Imm::from_bits(bits) }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{Datum, Global, Imm, Linkage, Module, Pic, Reloc, Type};
    use rucc_target::{TargetInfo, Triple};

    use super::Images;

    /// The images of a module with one read only global named `g`, holding that.
    ///
    /// The size comes from the data rather than from the caller, so that a test saying what is in
    /// the object does not also have to say how long it is and cannot say the two differently.
    fn images(build: impl Fn(&mut Module) -> Vec<Datum>) -> (Interner, Images) {
        images_on("x86_64-unknown-linux-gnu", build)
    }

    /// The same, for a module built for that target.
    fn images_on(triple: &str, build: impl Fn(&mut Module) -> Vec<Datum>) -> (Interner, Images) {
        let mut names = Interner::new();
        let target = TargetInfo::new(triple.parse::<Triple>().unwrap());
        let mut module = Module::new(names.intern("t.c"), &target);
        let data = build(&mut module);
        let size = data.iter().map(|datum| datum.size(&module)).sum();
        let mut global = Global::new(names.intern("g"), size, 8);
        global.linkage = Linkage::Internal;
        global.constant = true;
        global.init = Some(module.push_data(&data));
        module.add_global(global);
        let images = Images::of(&module, Pic::Executable);
        (names, images)
    }

    /// What a load of that type from that offset into `g` reads.
    fn read(names: &mut Interner, images: &Images, ty: Type, offset: u64) -> Option<Imm> {
        images.read(names.intern("g"), ty, offset)
    }

    /// A copy of a wide string on i386 is spelled four bytes at a time, since an eight byte store
    /// is one it has no instruction for. c-testsuite 00220, a `wchar_t` array initialized from a
    /// literal, did not compile at `-O2` before.
    #[test]
    fn a_copy_on_i386_is_spelled_a_word_at_a_time() {
        let (mut names, narrow) = images_on("i686-unknown-linux-gnu", |module| {
            vec![Datum::Bytes(module.push_bytes(b"h\0\0\0i\0\0\0\0\0\0\0"))]
        });
        let writes = narrow.spell(names.intern("g"), 0, 12).expect("every byte is known");
        let widths: Vec<u64> = writes
            .iter()
            .map(|write| match write {
                super::Write::Number(width, ..) | super::Write::Name(width, _) => *width,
                super::Write::Zero(bytes) => *bytes,
            })
            .collect();
        assert_eq!(widths, [4, 4, 4]);
        let (mut names, wide) =
            images(|module| vec![Datum::Bytes(module.push_bytes(b"h\0\0\0i\0\0\0\0\0\0\0"))]);
        let writes = wide.spell(names.intern("g"), 0, 12).expect("every byte is known");
        assert_eq!(writes.len(), 2, "eight and then four on x86-64, {writes:?}");
    }

    /// A `long long` in the object leaves the copy alone on i386, which has no 64-bit store.
    #[test]
    fn a_copy_on_i386_of_a_long_long_stays_a_copy() {
        let (mut names, images) = images_on("i686-unknown-linux-gnu", |module| {
            vec![Datum::Scalar {
                ty: Type::int(64),
                value: module.add_imm(Imm::int(7, Type::int(64))),
            }]
        });
        assert_eq!(images.spell(names.intern("g"), 0, 8), None);
    }

    /// The four bytes of `10, 20, 30, 40` as an `int` array is written, which is the case the
    /// whole pass exists for.
    #[test]
    fn a_slot_of_a_table_reads_what_the_table_was_initialized_to() {
        let (mut names, images) = images(|module| {
            [10i128, 20, 30, 40]
                .into_iter()
                .map(|value| Datum::Scalar {
                    ty: Type::int(32),
                    value: module.add_imm(Imm::int(value, Type::int(32))),
                })
                .collect()
        });
        let mut at = |offset| read(&mut names, &images, Type::int(32), offset).map(Imm::unsigned);
        assert_eq!(at(0), Some(10));
        assert_eq!(at(8), Some(30));
        assert_eq!(at(12), Some(40));
    }

    /// One byte of a string literal, which arrives as literal bytes rather than as scalars.
    #[test]
    fn a_byte_of_a_string_reads_the_byte_the_string_spells() {
        let (mut names, images) = images(|module| vec![Datum::Bytes(module.push_bytes(b"abc\0"))]);
        let mut at = |offset| read(&mut names, &images, Type::int(8), offset).map(Imm::unsigned);
        assert_eq!(at(0), Some(u128::from(b'a')));
        assert_eq!(at(1), Some(u128::from(b'b')));
        assert_eq!(at(3), Some(0));
        assert_eq!(at(4), None, "one past the end of the object");
    }

    /// Several bytes at once, which is the one place the target's byte order has anything to say.
    #[test]
    fn several_bytes_read_as_a_number_in_the_order_the_target_puts_them() {
        let (mut names, images) =
            images(|module| vec![Datum::Bytes(module.push_bytes(&[1, 2, 3, 4]))]);
        assert_eq!(
            read(&mut names, &images, Type::int(32), 0).map(Imm::unsigned),
            Some(0x0403_0201),
            "least significant byte first, which is what x86-64 is"
        );
    }

    /// A run of zeroes, which is how the tail of a partly initialized object is written.
    #[test]
    fn a_run_of_zeroes_reads_zero() {
        let (mut names, images) = images(|_| vec![Datum::Zero(16)]);
        assert_eq!(read(&mut names, &images, Type::int(64), 8).map(Imm::unsigned), Some(0));
    }

    /// Half of a scalar, which the image cannot answer because it does not hold the scalar as
    /// bytes and the question is about bytes.
    #[test]
    fn a_part_of_a_scalar_is_not_read() {
        let (mut names, images) = images(|module| {
            vec![Datum::Scalar {
                ty: Type::int(32),
                value: module.add_imm(Imm::int(0x0403_0201, Type::int(32))),
            }]
        });
        assert_eq!(read(&mut names, &images, Type::int(8), 0), None);
        assert_eq!(read(&mut names, &images, Type::int(16), 2), None);
        assert_eq!(
            read(&mut names, &images, Type::int(32), 0).map(Imm::unsigned),
            Some(0x0403_0201)
        );
    }

    /// An access starting in one piece and ending in the next, which is a number neither of them
    /// holds.
    #[test]
    fn an_access_that_crosses_from_one_piece_into_the_next_is_not_read() {
        let (mut names, images) = images(|module| {
            vec![Datum::Bytes(module.push_bytes(&[1, 2])), Datum::Bytes(module.push_bytes(&[3, 4]))]
        });
        assert_eq!(read(&mut names, &images, Type::int(32), 0), None);
        assert_eq!(read(&mut names, &images, Type::int(16), 0).map(Imm::unsigned), Some(0x0201));
        assert_eq!(read(&mut names, &images, Type::int(16), 2).map(Imm::unsigned), Some(0x0403));
    }

    /// The address of another symbol, which has no value until the link.
    #[test]
    fn the_address_of_something_else_is_not_read() {
        let (mut names, images) = images(|module| {
            let symbol = module.name;
            let to = module.add_reloc(Reloc { symbol, addend: 0, size: 8 });
            vec![Datum::Addr(to), Datum::Bytes(module.push_bytes(&[7]))]
        });
        assert_eq!(read(&mut names, &images, Type::PTR, 0), None);
        assert_eq!(
            read(&mut names, &images, Type::int(8), 8).map(Imm::unsigned),
            Some(7),
            "what follows a relocation is still where it was"
        );
    }

    /// Past the end of the object, which is a program that has already gone wrong and is not a
    /// program this answers.
    #[test]
    fn past_the_end_of_the_object_is_not_read() {
        let (mut names, images) = images(|_| vec![Datum::Zero(4)]);
        assert_eq!(read(&mut names, &images, Type::int(32), 4), None);
        assert_eq!(read(&mut names, &images, Type::int(64), 0), None);
    }

    /// A global something can write to, which is every global this does not look at.
    #[test]
    fn a_global_that_is_not_read_only_has_no_image() {
        let mut names = Interner::new();
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        let mut module = Module::new(names.intern("t.c"), &target);
        let mut global = Global::new(names.intern("g"), 4, 4);
        global.linkage = Linkage::Internal;
        global.init = Some(module.push_data(&[Datum::Zero(4)]));
        module.add_global(global);
        let images = Images::of(&module, Pic::Executable);
        assert!(images.is_empty());
        assert_eq!(images.read(names.intern("g"), Type::int(32), 0), None);
    }
}
