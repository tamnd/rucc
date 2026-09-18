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
//! Directly after the first `fold`, which is the pass that turns the subscript arithmetic above
//! into the offset this reads, and directly before `simplify`, which is what folds the arithmetic
//! standing on top of whatever this wrote. Those two neighbours are the whole of the position: one
//! ahead of it makes this possible and one behind it makes it worth doing.
//!
//! # Which globals are believed
//!
//! The four conditions `crate::extents` sets, which is [`crate::extents::vouched`], and one more.
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
//! The address of another symbol answers nothing, because a relocation has no value until the
//! link. Neither does an access that crosses from one piece of the image into the next, since
//! bytes spanning two of them are not a scalar either one holds, and neither does a `volatile`
//! access or an atomic one, whose whole point is that the access happens.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use rucc_base::Symbol;
use rucc_ir::{
    Block, Datum, Def, Extra, Flags, Func, Imm, Inst, MemOrder, Module, Opcode, Pic, Type, Value,
};

use crate::extents::vouched;
use crate::{Analyses, Analysis, Fuel, Pass, Preserved, Stats};

/// Recorded once for each load that became a constant.
const FOLDED: &str = "load from a read only object folded to what it was initialized to";

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
        let blocks: Vec<Block> = func.blocks().collect();
        for block in blocks {
            let insts: Vec<Inst> = func.insts(block).collect();
            for inst in insts {
                let Some((opcode, imm)) = answer(func, inst, images) else { continue };
                if !fuel.take() {
                    // Out of fuel, which is a request to stop transforming rather than to stop
                    // looking, per `crate::fold`.
                    stats.missed(NO_FUEL);
                    continue;
                }
                let at = func.add_imm(imm);
                let data = &mut func[inst];
                data.opcode = opcode;
                data.flags = Flags::NONE;
                data.args = rucc_ir::ValueList::EMPTY;
                data.extra = Extra::Imm(at);
                stats.optimized(FOLDED);
            }
        }
        stats
    }
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
    objects: HashMap<Symbol, Option<Object>>,
    /// Which end of a number the target puts first, which only the literal bytes need.
    little_endian: bool,
}

impl Images {
    /// The images of every read only global this module can vouch for.
    #[must_use]
    pub fn of(module: &Module, pic: Pic) -> Self {
        let mut objects: HashMap<Symbol, Option<Object>> = HashMap::new();
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
        Self { objects, little_endian: module.datalayout.little_endian }
    }

    /// Whether there is anything here to answer a load with.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    /// The value of type `ty` that lies `offset` bytes into the image of that global.
    #[must_use]
    pub fn read(&self, name: Symbol, ty: Type, offset: u64) -> Option<Imm> {
        let object = self.objects.get(&name)?.as_ref()?;
        let size = u64::from(ty.bits().div_ceil(8));
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
                return (at + width >= end)
                    .then(|| piece.read(ty, offset - at, size, self.little_endian))?;
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
                // Kept rather than dropped, so that what follows it is still at the offset it is
                // at. What it holds is an address the linker has not written yet.
                Datum::Addr(_) => Piece::Opaque(datum.size(module)),
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
    /// That many bytes whose value this cannot say.
    Opaque(u64),
}

impl Piece {
    /// How many bytes of the image it is.
    fn size(&self) -> u64 {
        match self {
            Self::Zero(bytes) | Self::Opaque(bytes) => *bytes,
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
            Self::Opaque(_) => None,
        }
    }
}

/// What this instruction reads, if it is a load the images can answer.
///
/// The opcode comes back with the value because a constant of an integer type and a constant of a
/// floating point type are two different instructions, and which one to write is decided by the
/// type of the load rather than by what the image turned out to hold.
fn answer(func: &Func, inst: Inst, images: &Images) -> Option<(Opcode, Imm)> {
    let data = &func[inst];
    if data.opcode != Opcode::Load || data.results != 1 || data.flags.contains(Flags::VOLATILE) {
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
    if !ty.is_scalar() || !(ty.is_int() || ty.is_float()) {
        return None;
    }
    let (base, offset) = address(func, *func[data.args].first()?)?;
    let Def::Result { inst: made, .. } = func[base].def else { return None };
    if func[made].opcode != Opcode::GlobalAddr {
        return None;
    }
    let Extra::Symbol(name) = func[made].extra else { return None };
    let imm = images.read(name, ty, u64::try_from(offset).ok()?)?;
    Some((if ty.is_int() { Opcode::IConst } else { Opcode::FConst }, imm))
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
        let mut names = Interner::new();
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
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
