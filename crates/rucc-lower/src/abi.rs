//! How a call travels, which is where the target's answer meets the walk.
//!
//! Design: `spec/12-abi-and-runtime.md` sections 12.1 to 12.5, and `spec/08-ir.md` section 8.9.
//!
//! `rucc-target` answers the question of how a value travels between a caller and a callee, and
//! it answers it over a shape: a size, an alignment and the scalars inside the object with the
//! offsets the layout gave them. Turning a C type into one of those is this file, because that
//! is the half of the question the C type system owns and `spec/18-package-layout.md` section
//! 18.2 keeps the other half out of here.
//!
//! What comes back is a [`Plan`], which is the IR signature of the call and, beside it, what
//! each value does to get there. The two are one answer and not two: the signature says a
//! function takes a `ptr sret(24, align 8)` and then two `i64`s, and the plan is what says the
//! first of those is where the return value is written and the other two are the halves of the
//! one structure the program wrote.
//!
//! # Why the plan and the signature are built together
//!
//! Three of these ABIs put an aggregate in memory once the registers it wanted are gone, so the
//! answer for one argument depends on every argument before it. There is no asking again later:
//! the classification is one pass over one call, the return value first, and everything that
//! wants to know the outcome reads what that pass wrote down.

use rucc_base::float::Format;
use rucc_ir::{Abi, Drains, Float, Param, Signature, Type};
use rucc_target::{
    Arg, Call, Convention, Kind, Narrow, Pass, Piece, Scalar, Shape, Slot, TargetInfo,
};
use rucc_tuple::{Arch, Os};
use rucc_types::{ArrayLen, RecordKind, TypeId, TypeKind, Types, float_format, layout};

use crate::repr;

/// The most scalars worth flattening out of an object no ABI here reads that many of.
///
/// A homogeneous floating point aggregate is at most four members, the x87 stack rule is at most
/// two, and the RISC-V rule is at most two. Everything above sixteen bytes that is none of those
/// travels the same way whatever is inside it, so an array of a thousand `char` is flattened far
/// enough to be over every one of those limits and no further.
const ENOUGH: usize = 17;

/// The size above which no ABI here classifies by what is inside the object, except through the
/// member counts [`ENOUGH`] is over.
const IN_REGISTERS: u64 = 16;

/// One value, flattened as far as an ABI reads it.
#[derive(Debug, Clone)]
pub(crate) enum Shaped {
    /// `void`, which is a return type and never an argument.
    Void,
    /// A scalar, which every ABI passes as itself.
    Scalar(Scalar),
    /// A `struct`, a `union`, an array or a `_Complex`.
    Aggregate {
        /// The size of the whole thing in bytes.
        size: u64,
        /// What it is aligned to.
        align: u64,
        /// The scalars in it, in offset order.
        pieces: Vec<Piece>,
        /// Whether it is a `_Complex` rather than a record of the same shape.
        complex: bool,
        /// Whether gcc gives it a floating point machine mode. See [`floating_mode`].
        floating: bool,
        /// Whether it is a vector, or a structure that holds only one. See [`single_vector`].
        vector: bool,
    },
}

impl Shaped {
    /// What the target is asked about.
    fn arg(&self) -> Arg<'_> {
        match self {
            Self::Void => Arg::Void,
            Self::Scalar(scalar) => Arg::Scalar(*scalar),
            Self::Aggregate { size, align, pieces, complex, floating, vector } => {
                Arg::Aggregate(Shape {
                    size: *size,
                    align: *align,
                    pieces,
                    complex: *complex,
                    floating: *floating,
                    vector: *vector,
                })
            }
        }
    }

    /// Its size and alignment in bytes, which is what a copy of it needs.
    fn extent(&self) -> (u64, u32) {
        match self {
            Self::Void => (0, 1),
            Self::Scalar(scalar) => (scalar.size, u32::try_from(scalar.align).unwrap_or(1).max(1)),
            Self::Aggregate { size, align, .. } => {
                (*size, u32::try_from(*align).unwrap_or(1).max(1))
            }
        }
    }
}

/// How one value travels, and what it takes in the IR to say so.
#[derive(Debug, Clone)]
pub(crate) struct Travel {
    /// What the target said.
    pub(crate) pass: Pass,
    /// The size of the object in bytes, for the passes that copy it.
    pub(crate) size: u64,
    /// What it is aligned to.
    pub(crate) align: u32,
    /// The IR types of the parameters it takes, in order, which is none for a value that does
    /// not travel and more than one for an object taken apart into registers.
    pub(crate) types: Vec<Type>,
    /// The C type this was classified from, which is what arrives rather than what the body
    /// works on.
    ///
    /// The two are the same everywhere a prototype says what a parameter is. They differ in an
    /// old style definition, `f(c) unsigned char c;`, where the call promotes what it passes
    /// because there is no prototype to convert it to, so an `int` arrives for a parameter the
    /// body reads as an `unsigned char`. Keeping it here is what lets the entry block convert
    /// the one into the other.
    pub(crate) ty: TypeId,
    /// The argument registers that going to memory left nothing in for the arguments after it.
    pub(crate) drains: Drains,
}

impl Travel {
    /// The registers the object is taken apart into, which is empty for every other pass.
    pub(crate) fn slots(&self) -> &[Slot] {
        match &self.pass {
            Pass::Pieces(slots) => slots,
            _ => &[],
        }
    }

    /// How many bytes the registers between them reach into, which is what a copy through them
    /// has to be able to hold.
    ///
    /// A twelve byte structure whose last four bytes travel in a register is read and written
    /// four bytes at a time and this is twelve. A five byte one is read as a whole register, and
    /// this is eight, which is three bytes more than the object: a load of eight bytes from it
    /// reads past the end, so the walk goes through a buffer of this size instead.
    pub(crate) fn reach(&self) -> u64 {
        self.slots().iter().map(|slot| slot.offset() + width(*slot)).max().unwrap_or(0)
    }
}

/// One call, classified: what the IR says about it and what each value does to get there.
#[derive(Debug, Clone)]
pub(crate) struct Plan {
    /// The signature, which is the sret parameter if there is one and then the parameters of
    /// every argument the callee's prototype named.
    pub(crate) signature: Signature,
    /// How the return value comes back.
    pub(crate) ret: Travel,
    /// How each argument travels, one per argument the call passes.
    pub(crate) args: Vec<Travel>,
    /// What the ABI asks of the values the signature does not name, one for each of them.
    ///
    /// Empty when they all travel as the values in hand, which is what a call with no arguments
    /// past its parameter list has and what nearly every other call has too. A structure the
    /// classification puts in the argument area is the one that does not, and it is the reason
    /// this is here: the bytes travel, and the `byval` that says so has no parameter to sit on.
    pub(crate) varargs: Vec<Abi>,
}

impl Plan {
    /// Whether the return value is written through a pointer the caller passes.
    pub(crate) fn returns_through_memory(&self) -> bool {
        matches!(self.ret.pass, Pass::Reference | Pass::Memory)
    }
}

/// Which of the three questions the classifier is being asked about one value.
///
/// The return value is asked about first, and the two kinds of argument are asked about in source
/// order. The third is separate from the second because two of the five ABIs answer it
/// differently: Darwin arm64 puts every argument past the `...` in the argument area whatever it
/// is, and x86-64 SysV wants the count of vector registers a variadic call used in `al`. Asking
/// [`Call::argument`] for one of those is asking for a different program, which is why this is an
/// enum and not a `bool` sitting next to another `bool`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Position {
    /// The value that comes back.
    Return,
    /// An argument the callee's prototype names.
    Fixed,
    /// An argument past the `...`, which only a variadic call has.
    Variadic,
}

/// Classifies one call and builds the IR signature for it.
///
/// `params` are the parameters the callee's type names and `actual` is what a call site passes,
/// which is longer than `params` for a variadic call and is empty for a definition, where the
/// question is only about the parameters. The error is what to report, which is a message rather
/// than a kind because there is exactly one thing every caller does with it.
///
/// `convention` is the callee's, which is the target's own unless its type says `ms_abi` or
/// `sysv_abi` asked for the other one. Only where things travel changes with it: what a type is,
/// its shape and its size, is still the target's, which is how gcc has an `ms_abi` function on
/// Linux take a `long double` that is still the x87 format, by reference because it is sixteen
/// bytes. The signature carries it on, so every end of the call below this reads the same one.
pub(crate) fn plan(
    types: &Types,
    target: &TargetInfo,
    convention: Convention,
    ret: TypeId,
    params: &[TypeId],
    actual: &[TypeId],
    variadic: bool,
) -> Result<Plan, &'static str> {
    // A target whose ABI is not described fails here. It is reported per call rather than refused
    // once at startup because that is where the compiler already has a span to point at, and
    // because everything short of a call still works there.
    let mut call = target.call_under(convention).ok_or("calling a function on this target")?;
    // Said before anything is asked, because on Windows on AArch64 the `...` changes how the named
    // arguments travel as well as the rest.
    if variadic {
        call = call.variadic();
    }
    let narrow = call.abi().narrow;
    let shaped = shape(types, target, ret).ok_or("returning a value of this type")?;
    let ret = travel(types, target, &mut call, &shaped, Position::Return, ret);

    let mut signature = Signature::new();
    signature.variadic = variadic;
    signature.convention = convention;
    if matches!(ret.pass, Pass::Reference | Pass::Memory) {
        let (size, align) = (ret.size, ret.align);
        signature.params.push(Param::with_abi(Type::PTR, Abi::Sret { size, align, popped: None }));
    } else {
        let returns = ret.types.iter().map(|ty| extended(types, target, narrow, &ret, *ty));
        signature.returns.extend(returns);
    }

    let count = params.len().max(actual.len());
    let mut args = Vec::with_capacity(count);
    let mut varargs = Vec::new();
    for index in 0..count {
        let ty = *params.get(index).or_else(|| actual.get(index)).expect("one of the two");
        // An object of no fixed size goes by reference on every target gcc has an answer for
        // there: `ix86_pass_by_reference` says so for any type without a constant size. The
        // caller copies it and passes where the copy is, which travels as any pointer does.
        let variable = repr::is_variable_length(types, ty);
        let shaped = if variable {
            let word = u64::from(target.pointer_width / 8);
            Shaped::Scalar(Scalar { kind: Kind::Integer, size: word, align: word })
        } else {
            shape(types, target, ty).ok_or("passing a value of this type")?
        };
        // Past the parameter list is past the `...`, and only if the callee has one. A call with
        // more arguments than parameters and no `...` is a program with no prototype in scope,
        // where every argument is a fixed one that nothing declared.
        let position =
            if variadic && index >= params.len() { Position::Variadic } else { Position::Fixed };
        let mut travel = travel(types, target, &mut call, &shaped, position, ty);
        if variable {
            travel.pass = Pass::Reference;
            travel.types = vec![Type::PTR];
            travel.align = repr::align_of(types, target, ty);
        }
        if index < params.len() {
            let params =
                travel.types.iter().map(|ty| extended(types, target, narrow, &travel, *ty));
            signature.params.extend(params);
        } else {
            // An argument past the parameter list travels the same way and has nowhere in the
            // signature to say so, which is what the call carries its own list for.
            varargs.extend(travel.types.iter().map(|ty| param(&travel, *ty).abi));
        }
        args.push(travel);
    }
    if varargs.iter().all(|abi| *abi == Abi::Plain) {
        varargs.clear();
    }
    Ok(Plan { signature, ret, args, varargs })
}

/// The parameter one of a travel's IR types becomes.
fn param(travel: &Travel, ty: Type) -> Param {
    match travel.pass {
        // The object's own bytes go in the argument area and the pointer is where they are read
        // from, which is what `byval` means and is why the size and the alignment are on it.
        Pass::Memory => {
            let Travel { size, align, drains, .. } = *travel;
            Param::with_abi(ty, Abi::ByVal { size, align, drains })
        }
        _ => Param::new(ty),
    }
}

/// [`param`], and on an ABI that extends an integer narrower than an `int` the side it is
/// extended from.
///
/// Only a C integer, `bool` and enumeration included, and only one travelling as itself. A one
/// byte structure travels in a register as well and nobody extends it, because it has no sign
/// to extend by, and one past the `...` goes to the argument area on the only ABI that says this,
/// which is why a variadic call's own list never needs it.
fn extended(
    types: &Types,
    target: &TargetInfo,
    narrow: Narrow,
    travel: &Travel,
    ty: Type,
) -> Param {
    let integer = matches!(
        types.kind(types.canonical(travel.ty)),
        TypeKind::Bool | TypeKind::Int(_) | TypeKind::BitInt { .. } | TypeKind::Enum(_)
    );
    let direct = matches!(travel.pass, Pass::Direct) && ty.is_int() && !ty.is_vector();
    if narrow != Narrow::ToInt || !integer || !direct || ty.bits() >= 32 {
        return param(travel, ty);
    }
    let abi = if repr::is_signed(types, target, travel.ty) { Abi::Sext } else { Abi::Zext };
    Param::with_abi(ty, abi)
}

/// Asks the target about one value and works out what the IR needs to say it.
fn travel(
    types: &Types,
    target: &TargetInfo,
    call: &mut Call,
    shaped: &Shaped,
    position: Position,
    ty: TypeId,
) -> Travel {
    let (size, mut align) = shaped.extent();
    let arg = shaped.arg();
    let left = (call.integer_left(), call.float_left());
    let pass = match position {
        Position::Return => call.returns(&arg),
        Position::Fixed => call.argument(&arg),
        Position::Variadic => call.variadic_argument(&arg),
    };
    // Where the ABI packs the argument area, the alignment an object in it gets is the ABI's
    // answer rather than the type's. Only the alignment, because the size is also how many bytes
    // the caller copies there, and the backend takes the size up to the alignment on its own.
    if let (Position::Fixed, Pass::Memory, Arg::Aggregate(shape)) = (position, &pass, &arg) {
        align = u32::try_from(call.in_memory(shape)).unwrap_or(align);
    }
    let types = match &pass {
        Pass::Ignore => Vec::new(),
        Pass::Direct => match repr::value_type(types, target, ty) {
            Some(ty) => vec![ty],
            // A scalar the IR has no type for, which is `__bf16` today. It is reported wherever
            // it is used rather than here, and a pointer keeps the shape of the walk.
            None => vec![Type::PTR],
        },
        Pass::Pieces(slots) => slots.iter().map(|slot| slot_type(*slot)).collect(),
        Pass::Reference | Pass::Memory => vec![Type::PTR],
    };
    // An object that went to memory and took every register of a kind with it is AAPCS64's
    // rule for one that found too few left, and the backend has to know so the next argument of
    // that kind goes to memory too. Only a kind that had some left can have been drained. One
    // that took a single register and left some is i386 `fastcall`'s small structure.
    let drains = match pass {
        Pass::Memory if left.1 > 0 && call.float_left() == 0 => Drains::Floats,
        Pass::Memory if left.0 > 0 && call.integer_left() == 0 => Drains::Integers,
        Pass::Memory if call.integer_left() + 1 == left.0 => Drains::OneInteger,
        _ => Drains::Nothing,
    };
    Travel { pass, size, align, types, ty, drains }
}

/// The IR type one register's worth of an object is read as.
///
/// A slot as wide as a register is that register's integer type. One that is not, which is what
/// the last eightbyte of a twelve byte structure is, is rounded up to the next width a machine
/// has an instruction for, and the walk is what keeps the load from reading past the object.
///
/// A slot of sixteen bytes is an `i128`. Only the wasm32 ABI makes one, for a structure whose one
/// member is an `__int128`. clang passes that structure as the member, and the wasm backend holds
/// an `i128` in two `i64` values, so the structure travels as the bare `__int128` does.
pub(crate) fn slot_type(slot: Slot) -> Type {
    match slot {
        Slot::Integer { size, .. } => Type::int(size.next_power_of_two().clamp(1, 16) * 8),
        // A vector of `size` bytes, which the wasm backend holds in one `v128`. A vector of one
        // byte is two lanes, because a type of one lane is a scalar, and the walk is what keeps
        // the second lane from reaching past the object.
        Slot::Vector { size, .. } => Type::vector(Type::int(8), size.max(2)),
        Slot::Float { format, .. } => match repr::ir_format(format) {
            Some(format) => Type::float(format),
            // A format the IR has no type for, which is `__bf16` in an aggregate. Sixteen bits
            // of it travel in whatever holds sixteen bits.
            None => Type::float(Float::F16),
        },
    }
}

/// How many bytes a load or a store of one slot touches.
pub(crate) fn width(slot: Slot) -> u64 {
    let ty = slot_type(slot);
    u64::from((ty.bits() * ty.lanes()).div_ceil(8))
}

/// The registers an object read off a variable argument list arrived in, which is empty for one
/// the classification sent to the argument area and for a type it has nothing to say about.
///
/// The classification is asked with nothing spent, because what a `va_arg` needs from it is how
/// many registers of each file the object takes and which of its bytes are in each of them.
/// Which registers those were is not a thing the callee can work out from the type: the caller
/// may have passed anything at all ahead of it, and the offsets in the list are what say where
/// the object ended up. So this is the shape of the answer and the list holds the rest of it.
///
/// An `__int128` is asked about as the two words it is. Asked as a scalar, the classification
/// says it travels as itself, which is true and says nothing about where either half is, and the
/// psABI's answer is the one it gives a sixteen byte aligned pair of `long`s: two general purpose
/// registers when both are left and the argument area on a sixteen byte boundary when they are
/// not.
pub(crate) fn va_slots(types: &Types, target: &TargetInfo, ty: TypeId) -> Vec<Slot> {
    let Some(shaped) = shape(types, target, ty) else { return Vec::new() };
    // Asked as an argument of a variadic function, which it is, and which on Windows on AArch64 is
    // what sends a structure of four `double`s by reference rather than in four vector registers.
    let Some(call) = target.call() else { return Vec::new() };
    let mut call = call.variadic();
    let word = u64::from(target.pointer_width / 8);
    let halves;
    let arg = match shaped {
        Shaped::Scalar(scalar) if scalar.kind == Kind::Integer && scalar.size == 2 * word => {
            let half = Scalar { kind: Kind::Integer, size: word, align: word };
            halves = [Piece { offset: 0, scalar: half }, Piece { offset: word, scalar: half }];
            Arg::Aggregate(Shape {
                size: scalar.size,
                align: scalar.align,
                pieces: &halves,
                complex: false,
                floating: false,
                vector: false,
            })
        }
        _ => shaped.arg(),
    };
    match call.argument(&arg) {
        Pass::Pieces(slots) => slots,
        _ => Vec::new(),
    }
}

/// Flattens a C type into what an ABI reads, and [`None`] for one it cannot describe.
pub(crate) fn shape(types: &Types, target: &TargetInfo, ty: TypeId) -> Option<Shaped> {
    let id = types.canonical(ty);
    if matches!(types.kind(id), TypeKind::Void) {
        return Some(Shaped::Void);
    }
    if let Some(chunks) = chunks(types, target, id) {
        return Some(chunks);
    }
    if let Some(scalar) = scalar(types, target, id) {
        return Some(Shaped::Scalar(scalar));
    }
    let size = repr::size_of(types, target, id);
    let align = u64::from(repr::align_of(types, target, id));
    // A vector is each of its lanes on wasm, however many there are, so its pieces are not cut
    // there. Every other target reads no more than [`ENOUGH`] of them.
    let vector = single_vector(types, target, id);
    let capped = size > IN_REGISTERS && !(vector && target.tuple.arch() == Arch::Wasm32);
    let mut flatten = Flatten { types, target, pieces: Vec::new(), capped };
    flatten.push(id, 0)?;
    let mut pieces = flatten.pieces;
    // Offset order is what every rule is written over, and a union is what puts two members at
    // one offset. Two members that are the same thing in the same place are one piece, which is
    // what makes a union of two `float`s the homogeneous aggregate AAPCS64 says it is.
    pieces.sort_by_key(|piece| piece.offset);
    pieces.dedup();
    let complex = matches!(types.kind(id), TypeKind::Complex(_));
    let floating = floating_mode(types, target, id);
    Some(Shaped::Aggregate { size, align, pieces, complex, floating, vector })
}

/// Whether a type is a GNU vector, or a structure or a union that clang passes as the vector it
/// holds.
///
/// That is clang's single element structure: a `struct` or a `union` whose one member with bytes
/// in it fills it and is such a type itself, through any number of structures, unions and arrays
/// of one element. A union of two vectors is not one, and clang 23 passes it as the address of a
/// copy. Only the wasm rule asks, see [`rucc_abi::Shape::vector`].
fn single_vector(types: &Types, target: &TargetInfo, ty: TypeId) -> bool {
    let id = types.canonical(ty);
    match types.kind(id) {
        TypeKind::Vector { .. } => true,
        TypeKind::Array { elem, len: ArrayLen::Fixed(1) } => single_vector(types, target, elem),
        TypeKind::Record(record) => {
            let info = types.record_info(record);
            let size = repr::size_of(types, target, id);
            let mut members = info.fields.iter().filter(|field| {
                field.bits != Some(0) && repr::size_of(types, target, field.ty) > 0
            });
            match (members.next(), members.next()) {
                (Some(field), None) => {
                    field.bits.is_none()
                        && field.offset == 0
                        && repr::size_of(types, target, field.ty) == size
                        && single_vector(types, target, field.ty)
                }
                _ => false,
            }
        }
        _ => false,
    }
}

/// Whether gcc gives a type a floating point machine mode, which is what i386 `fastcall` asks of a
/// structure argument: one that has such a mode takes no register, and every other one takes one
/// for each of its words.
///
/// A floating point scalar and a `_Complex` have one. A structure has the mode of its member when
/// it has exactly one member with bytes in it and that member fills it, and an array of one
/// element has the mode of the element. A `union` never has one, whatever is in it, which is the
/// one place this and the pieces disagree: `union { float f; }` takes a register where
/// `struct { float f; }` does not. Measured with i686-linux-gnu-gcc and i686-w64-mingw32-gcc 13,
/// which agree. tamnd/rucc#3029.
fn floating_mode(types: &Types, target: &TargetInfo, ty: TypeId) -> bool {
    let id = types.canonical(ty);
    match types.kind(id) {
        TypeKind::Float(_) | TypeKind::Complex(_) => true,
        TypeKind::Atomic(inner) => floating_mode(types, target, inner),
        TypeKind::Array { elem, len: ArrayLen::Fixed(1) } => floating_mode(types, target, elem),
        TypeKind::Record(record) => {
            let info = types.record_info(record);
            if info.kind != RecordKind::Struct {
                return false;
            }
            let size = repr::size_of(types, target, id);
            let mut members = info.fields.iter().filter(|field| {
                field.bits != Some(0) && repr::size_of(types, target, field.ty) > 0
            });
            match (members.next(), members.next()) {
                (Some(field), None) => {
                    field.bits.is_none()
                        && field.offset == 0
                        && repr::size_of(types, target, field.ty) == size
                        && floating_mode(types, target, field.ty)
                }
                _ => false,
            }
        }
        _ => false,
    }
}

/// A `_BitInt` wider than a register, as the x86-64 psABI reads it, and [`None`] for anything
/// else.
///
/// The psABI says a `_BitInt(N)` wider than sixty four bits is classified as if it were a
/// structure of `long`s, as many as it takes. That is not the rule an `__int128` has, though the
/// two are the same size up to a hundred and twenty eight bits: the `_BitInt` is aligned to eight
/// rather than sixteen, so when it does not get its pair of registers it sits in the argument
/// area at the next eight byte boundary rather than the next sixteen. gcc 16.2.0 agrees, with
/// `_Alignof(_BitInt(65))` and `_Alignof(_BitInt(128))` both eight. Asked as a scalar, the
/// classification would give it the `__int128` answer, so it is asked as the structure instead,
/// and the walk then splits the value into its words on the way out and joins them on the way
/// in, which it already does for a scalar that travels in pieces.
///
/// Only on x86-64 outside Windows. Windows passes anything over eight bytes as the address of a
/// copy whatever it is, and the other machines have rules of their own that tamnd/rucc#425 has
/// not been taught yet.
fn chunks(types: &Types, target: &TargetInfo, id: TypeId) -> Option<Shaped> {
    if !matches!(types.kind(id), TypeKind::BitInt { .. })
        || target.tuple.arch() != Arch::X86_64
        || target.tuple.os() == Os::Windows
    {
        return None;
    }
    let size = repr::size_of(types, target, id);
    if size <= 8 {
        return None;
    }
    let word = Scalar { kind: Kind::Integer, size: 8, align: 8 };
    let pieces = (0..size / 8).map(|at| Piece { offset: at * 8, scalar: word }).collect();
    let align = u64::from(repr::align_of(types, target, id));
    Some(Shaped::Aggregate { size, align, pieces, complex: false, floating: false, vector: false })
}

/// The scalar a C type is, and [`None`] for a type that is not one.
fn scalar(types: &Types, target: &TargetInfo, ty: TypeId) -> Option<Scalar> {
    let id = types.canonical(ty);
    let kind = match types.kind(id) {
        TypeKind::Bool
        | TypeKind::Int(_)
        | TypeKind::BitInt { .. }
        | TypeKind::Enum(_)
        | TypeKind::Pointer(_) => Kind::Integer,
        TypeKind::Float(kind) => Kind::Float(float_format(kind, target)),
        TypeKind::Atomic(inner) => return scalar(types, target, inner),
        _ => return None,
    };
    let layout = layout(types, id, target).ok()?;
    Some(Scalar { kind, size: layout.size, align: layout.align })
}

/// The walk that takes an aggregate apart into the scalars in it.
struct Flatten<'a> {
    types: &'a Types,
    target: &'a TargetInfo,
    pieces: Vec<Piece>,
    /// Whether the object is one no ABI here reads the members of past a certain number of them.
    capped: bool,
}

impl Flatten<'_> {
    /// Whether enough of the object has been taken apart to answer every question about it.
    fn full(&self) -> bool {
        self.capped && self.pieces.len() >= ENOUGH
    }

    /// A vector an ABI reads as one value rather than as its lanes, and [`None`] for any other
    /// type.
    ///
    /// On x86-64 a vector of eight or sixteen bytes is one value. The psABI gives the eight byte
    /// ones, `__m64` and its kind, the class SSE whatever the lanes are, and the sixteen byte
    /// ones, `__m128` and its kind, SSE and then SSEUP, which is one xmm register for the whole
    /// of it. So it is one floating point piece of its own size, which is exactly the shape a
    /// `_Float128` already has and already travels in one register for, and the classification
    /// needs nothing new. gcc 16.2.0 puts a `vector_size(8)` of one `long` in xmm0 and a
    /// `struct { v2f a; int b; }` in xmm0 and rdi, which is this rule read both ways.
    ///
    /// A narrower vector is left to its lanes, which makes it INTEGER, and gcc 16.2.0 agrees: a
    /// `vector_size(4)` of `char` arrives in edi. A wider one is over sixteen bytes and goes to
    /// memory whatever its pieces say. Windows x64 passes an aggregate by its size alone, so the
    /// piece it is made of changes nothing there. Other machines still take a vector apart into
    /// lanes, which on AArch64 is not where gcc puts a short vector either, and that half of
    /// tamnd/rucc#1140 is still open.
    fn whole_vector(&self, id: TypeId) -> Option<Scalar> {
        if !matches!(self.types.kind(id), TypeKind::Vector { .. })
            || self.target.tuple.arch() != Arch::X86_64
        {
            return None;
        }
        let size = repr::size_of(self.types, self.target, id);
        let format = match size {
            8 => Format::Double,
            16 => Format::Quad,
            _ => return None,
        };
        let align = u64::from(repr::align_of(self.types, self.target, id));
        Some(Scalar { kind: Kind::Float(format), size, align })
    }

    /// Everything in one type, at its offset from the start of the object.
    fn push(&mut self, ty: TypeId, at: u64) -> Option<()> {
        if self.full() {
            return Some(());
        }
        let id = self.types.canonical(ty);
        if let Some(scalar) = scalar(self.types, self.target, id).or_else(|| self.whole_vector(id))
        {
            self.pieces.push(Piece { offset: at, scalar });
            return Some(());
        }
        match self.types.kind(id) {
            // The two halves, each at its own offset. A half is a scalar whatever its type is,
            // so the class comes from the half rather than from the keyword in front of it, and
            // `_Complex int` is two integers where `_Complex double` is two doubles.
            TypeKind::Complex(part) => {
                let scalar = scalar(self.types, self.target, part)?;
                self.pieces.push(Piece { offset: at, scalar });
                self.pieces.push(Piece { offset: at + scalar.size, scalar });
            }
            // A vector [`Flatten::whole_vector`] did not take comes apart into its lanes the
            // way an array of them would.
            TypeKind::Vector { elem, len } => {
                let stride = repr::size_of(self.types, self.target, elem);
                for index in 0..u64::from(len) {
                    self.push(elem, at + index * stride)?;
                    if self.full() {
                        break;
                    }
                }
            }
            TypeKind::Array { elem, len } => {
                // An array of unknown length is the flexible array member at the end of a
                // record, which is not part of the object a call copies.
                let count = match len {
                    ArrayLen::Fixed(count) => count,
                    ArrayLen::Unknown => 0,
                    // A variable length array, whose size nobody has yet.
                    ArrayLen::Variable(_) | ArrayLen::Star => return None,
                };
                let stride = repr::size_of(self.types, self.target, elem);
                for index in 0..count {
                    self.push(elem, at + index * stride)?;
                    if self.full() {
                        break;
                    }
                }
            }
            TypeKind::Record(id) => {
                let fields = self.types.record_info(id).fields.clone();
                for field in fields {
                    match field.bits {
                        // A zero width bit-field is a boundary and not a member, and nothing of
                        // the object is in it.
                        Some(0) => {}
                        // A bit-field is an integer wherever it starts, which is what an
                        // alignment of one says: it is the one member `packed` cannot send the
                        // whole aggregate to memory over.
                        Some(bits) => {
                            let size = (u64::from(field.bit) + u64::from(bits)).div_ceil(8);
                            let scalar = Scalar { kind: Kind::Integer, size, align: 1 };
                            self.pieces.push(Piece { offset: at + field.offset, scalar });
                        }
                        None => self.push(field.ty, at + field.offset)?,
                    }
                    if self.full() {
                        break;
                    }
                }
            }
            // A function, an incomplete type, or something else with no bytes to pass.
            _ => return None,
        }
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use rucc_types::{FieldDecl, FloatKind, IntKind, RecordKind, RecordOptions, layout_record};

    use super::*;

    fn target(triple: &str) -> TargetInfo {
        TargetInfo::new(triple.parse().expect("a triple the compiler supports"))
    }

    /// A record of these members, laid out.
    fn record(types: &mut Types, target: &TargetInfo, members: &[TypeId]) -> TypeId {
        let fields: Vec<FieldDecl> = members.iter().map(|ty| FieldDecl::new(None, *ty)).collect();
        let id = types.declare_record(RecordKind::Struct, None);
        let options = RecordOptions::default();
        let laid = layout_record(types, RecordKind::Struct, &fields, &options, target)
            .expect("a record that lays out");
        types.complete_record(id, laid);
        types.record(id)
    }

    /// A union of these members, laid out.
    fn union(types: &mut Types, target: &TargetInfo, members: &[TypeId]) -> TypeId {
        let fields: Vec<FieldDecl> = members.iter().map(|ty| FieldDecl::new(None, *ty)).collect();
        let id = types.declare_record(RecordKind::Union, None);
        let options = RecordOptions::default();
        let laid = layout_record(types, RecordKind::Union, &fields, &options, target)
            .expect("a union that lays out");
        types.complete_record(id, laid);
        types.record(id)
    }

    #[test]
    fn a_structure_is_flattened_into_the_scalars_an_abi_reads() {
        let mut types = Types::new();
        let target = target("x86_64-unknown-linux-gnu");
        let int = types.int(IntKind::Int);
        let double = types.float(FloatKind::Double);
        let id = record(&mut types, &target, &[int, double]);
        let Some(Shaped::Aggregate { size, pieces, .. }) = shape(&types, &target, id) else {
            panic!("a record is an aggregate");
        };
        assert_eq!(size, 16);
        assert_eq!(pieces.len(), 2);
        assert_eq!(pieces[0].offset, 0);
        // Where the second one is, which is what the padding after the `int` decides and what a
        // reader of the slots cannot work out for itself.
        assert_eq!(pieces[1].offset, 8);
    }

    #[test]
    fn an_object_no_abi_reads_the_members_of_is_not_taken_all_the_way_apart() {
        let mut types = Types::new();
        let target = target("x86_64-unknown-linux-gnu");
        let char_ty = types.int(IntKind::Char);
        let array = types.array(char_ty, ArrayLen::Fixed(4096));
        let id = record(&mut types, &target, &[array]);
        let Some(Shaped::Aggregate { size, pieces, .. }) = shape(&types, &target, id) else {
            panic!("a record is an aggregate");
        };
        assert_eq!(size, 4096);
        assert_eq!(pieces.len(), ENOUGH);

        // And the answer is the same one four thousand pieces would have given, because every
        // rule that reads them is over a member count this is already past.
        let plan = plan(&types, &target, Convention::Target, types.void(), &[id], &[], false)
            .expect("a plan");
        assert_eq!(plan.args[0].pass, Pass::Memory);
    }

    #[test]
    fn a_structure_that_travels_in_registers_says_which_bytes_each_one_holds() {
        let mut types = Types::new();
        let target = target("x86_64-unknown-linux-gnu");
        let int = types.int(IntKind::Int);
        let double = types.float(FloatKind::Double);
        let id = record(&mut types, &target, &[int, double]);
        let plan =
            plan(&types, &target, Convention::Target, id, &[id], &[], false).expect("a plan");
        // Sixteen bytes, an `int` in the first eightbyte and a `double` in the second, which is
        // one general purpose register and one vector register both going in and coming back.
        assert_eq!(plan.args[0].types, vec![Type::int(64), Type::float(Float::F64)]);
        assert_eq!(plan.args[0].slots()[1].offset(), 8);
        assert_eq!(plan.ret.types, vec![Type::int(64), Type::float(Float::F64)]);
        assert!(!plan.returns_through_memory());
        assert_eq!(plan.signature.params.len(), 2);
    }

    /// tamnd/rucc#1140. A sixteen byte vector is SSE and SSEUP on x86-64, one xmm register going
    /// in and coming back, whatever its lanes are, and an eight byte one is SSE even when its
    /// lane is a `long`. A vector inside a structure is classified with the rest of it, so
    /// `struct { v2f a; int b; }` is one xmm register and one general purpose register. A four
    /// byte vector stays INTEGER, which is where gcc 16.2.0 puts it too.
    #[test]
    fn a_vector_on_x86_64_is_one_vector_register_whatever_its_lanes_are() {
        let mut types = Types::new();
        let target = target("x86_64-unknown-linux-gnu");
        let int = types.int(IntKind::Int);
        let long = types.int(IntKind::Long);
        let char_ty = types.int(IntKind::Char);
        let float = types.float(FloatKind::Float);
        let v4i = types.vector(int, 4);
        let v1l = types.vector(long, 1);
        let v2f = types.vector(float, 2);
        let v4c = types.vector(char_ty, 4);
        let quad = vec![Type::float(Float::F128)];
        let double = vec![Type::float(Float::F64)];
        for (ty, want) in [(v4i, &quad), (v1l, &double), (v2f, &double)] {
            let planned =
                plan(&types, &target, Convention::Target, ty, &[ty], &[], false).expect("a plan");
            assert_eq!(&planned.args[0].types, want);
            assert_eq!(&planned.ret.types, want);
        }
        let planned =
            plan(&types, &target, Convention::Target, v4c, &[v4c], &[], false).expect("a plan");
        assert_eq!(planned.args[0].types, vec![Type::int(32)]);
        let mixed = record(&mut types, &target, &[v2f, int]);
        let planned = plan(&types, &target, Convention::Target, types.void(), &[mixed], &[], false)
            .expect("a plan");
        assert_eq!(planned.args[0].types, vec![Type::float(Float::F64), Type::int(64)]);
    }

    /// A `_BitInt` wider than a register is a structure of `long`s to the x86-64 psABI, so it
    /// travels as two words both ways, and once the registers are gone it goes to the argument
    /// area aligned to eight, where an `__int128` would be aligned to sixteen. gcc 16.2.0 says
    /// `_Alignof(_BitInt(128))` is eight. AArch64 has not been taught this and keeps the scalar.
    #[test]
    fn a_bit_int_wider_than_a_register_on_x86_64_is_a_structure_of_words() {
        let mut types = Types::new();
        let x86 = target("x86_64-unknown-linux-gnu");
        let b65 = types.bit_int(true, 65);
        let b128 = types.bit_int(true, 128);
        let words = vec![Type::int(64), Type::int(64)];
        let planned =
            plan(&types, &x86, Convention::Target, b65, &[b65], &[], false).expect("a plan");
        assert_eq!(planned.args[0].types, words);
        assert_eq!(planned.ret.types, words);
        let long = types.int(IntKind::Long);
        let mut params = vec![long; 6];
        params.push(b128);
        let planned = plan(&types, &x86, Convention::Target, types.void(), &params, &[], false)
            .expect("a plan");
        assert!(matches!(planned.args[6].pass, Pass::Memory), "the registers are gone");
        assert_eq!(planned.args[6].align, 8, "and it is aligned as a structure of words");
        let arm = target("aarch64-unknown-linux-gnu");
        let planned =
            plan(&types, &arm, Convention::Target, b65, &[b65], &[], false).expect("a plan");
        assert_eq!(planned.args[0].types, vec![Type::int(65)]);
    }

    #[test]
    fn a_return_value_too_large_for_the_registers_becomes_the_first_parameter() {
        let mut types = Types::new();
        let target = target("x86_64-unknown-linux-gnu");
        let double = types.float(FloatKind::Double);
        let id = record(&mut types, &target, &[double, double, double]);
        let plan = plan(&types, &target, Convention::Target, id, &[], &[], false).expect("a plan");
        assert!(plan.returns_through_memory());
        assert!(plan.signature.returns.is_empty());
        assert_eq!(plan.signature.params.len(), 1);
        assert_eq!(plan.signature.params[0].abi, Abi::Sret { size: 24, align: 8, popped: None });
    }

    #[test]
    fn an_object_that_finds_too_few_registers_on_aarch64_leaves_none_of_that_kind_behind_it() {
        let mut types = Types::new();
        let target = target("aarch64-unknown-linux-gnu");
        let (float, double) = (types.float(FloatKind::Float), types.float(FloatKind::Double));
        let long = types.int(IntKind::Long);
        let void = types.void();
        let hfa = record(&mut types, &target, &[float, float, float]);
        let pair = record(&mut types, &target, &[long, long]);
        let bytes = |drains| Abi::ByVal { size: 12, align: 4, drains };
        // Six doubles leave two vector registers, and three floats need three.
        let mut params = vec![double; 6];
        params.extend([hfa, float]);
        let planned =
            plan(&types, &target, Convention::Target, void, &params, &[], false).expect("a plan");
        assert_eq!(planned.signature.params[6].abi, bytes(Drains::Floats));
        // With none left to begin with there is nothing for the object to drain.
        let mut params = vec![double; 8];
        params.extend([hfa, float]);
        let planned =
            plan(&types, &target, Convention::Target, void, &params, &[], false).expect("a plan");
        assert_eq!(planned.signature.params[8].abi, bytes(Drains::Nothing));
        // Seven longs leave one general purpose register, and the pair needs two.
        let mut params = vec![long; 7];
        params.extend([pair, long]);
        let planned =
            plan(&types, &target, Convention::Target, void, &params, &[], false).expect("a plan");
        let drained = Abi::ByVal { size: 16, align: 8, drains: Drains::Integers };
        assert_eq!(planned.signature.params[7].abi, drained);
    }

    #[test]
    fn a_small_structure_spends_one_fastcall_register_and_a_bigger_one_both() {
        let mut types = Types::new();
        let target = target("i686-linux-gnu");
        let int = types.int(IntKind::Int);
        let small = record(&mut types, &target, &[int]);
        let big = record(&mut types, &target, &[int, int]);
        // `fastcall int f(struct { int a; }, int)`, where gcc has the `int` in edx.
        let planned = plan(&types, &target, Convention::Fastcall, int, &[small, int], &[], false)
            .expect("a plan");
        let spent = Abi::ByVal { size: 4, align: 4, drains: Drains::OneInteger };
        assert_eq!(planned.signature.params[0].abi, spent);
        // `fastcall int f(struct { int a, b; }, int)`, where gcc has the `int` on the stack.
        let planned = plan(&types, &target, Convention::Fastcall, int, &[big, int], &[], false)
            .expect("a plan");
        let drained = Abi::ByVal { size: 8, align: 4, drains: Drains::Integers };
        assert_eq!(planned.signature.params[0].abi, drained);
    }

    /// tamnd/rucc#3029. What a `fastcall` structure spends goes by the mode gcc gives it, which a
    /// `union` and a wrapped `_Complex` have the other way round from the pieces in them.
    #[test]
    fn a_fastcall_union_spends_registers_and_a_wrapped_complex_does_not() {
        let mut types = Types::new();
        let target = target("i686-linux-gnu");
        let int = types.int(IntKind::Int);
        let float = types.float(FloatKind::Float);
        let double = types.float(FloatKind::Double);
        let one = types.array(float, ArrayLen::Fixed(1));
        let complex = types.complex_float(FloatKind::Float);
        let union_float = union(&mut types, &target, &[float]);
        let union_double = union(&mut types, &target, &[double]);
        let wrapped_union = record(&mut types, &target, &[union_float]);
        let wrapped_complex = record(&mut types, &target, &[complex]);
        let wrapped_float = record(&mut types, &target, &[float]);
        let wrapped_array = record(&mut types, &target, &[one]);
        let wrapped_twice = record(&mut types, &target, &[wrapped_float]);
        let two = record(&mut types, &target, &[float, float]);
        let cases = [
            (union_float, 4, Drains::OneInteger),
            (wrapped_union, 4, Drains::OneInteger),
            (union_double, 8, Drains::Integers),
            (two, 8, Drains::Integers),
            (wrapped_complex, 8, Drains::Nothing),
            (wrapped_float, 4, Drains::Nothing),
            (wrapped_array, 4, Drains::Nothing),
            (wrapped_twice, 4, Drains::Nothing),
        ];
        for (ty, size, drains) in cases {
            let planned = plan(&types, &target, Convention::Fastcall, int, &[ty, int], &[], false)
                .expect("a plan");
            assert_eq!(planned.signature.params[0].abi, Abi::ByVal { size, align: 4, drains });
        }
    }

    #[test]
    fn a_structure_passed_past_a_parameter_list_says_so_on_the_call_and_not_the_signature() {
        let mut types = Types::new();
        let target = target("x86_64-unknown-linux-gnu");
        let double = types.float(FloatKind::Double);
        let int = types.int(IntKind::Int);
        let big = record(&mut types, &target, &[double, double, double]);
        let ptr = types.pointer(types.int(IntKind::Char));
        // `int p(const char *, ...)` called as `p("", 1, v)`, where `v` is the structure. The
        // first is the parameter the prototype names and the other two are past it.
        let plan = plan(&types, &target, Convention::Target, int, &[ptr], &[ptr, int, big], true)
            .expect("a plan");
        assert_eq!(plan.signature.params.len(), 1);
        assert_eq!(plan.args[2].pass, Pass::Memory);
        assert_eq!(
            plan.varargs,
            vec![Abi::Plain, Abi::ByVal { size: 24, align: 8, drains: Drains::Nothing }]
        );
    }

    #[test]
    fn a_call_whose_arguments_all_travel_as_themselves_says_nothing_about_them() {
        let mut types = Types::new();
        let target = target("x86_64-unknown-linux-gnu");
        let int = types.int(IntKind::Int);
        let ptr = types.pointer(types.int(IntKind::Char));
        let plan = plan(&types, &target, Convention::Target, int, &[ptr], &[ptr, int, int], true)
            .expect("a plan");
        assert!(plan.varargs.is_empty());
    }

    #[test]
    fn what_a_structure_reaches_into_is_not_always_what_it_is() {
        let mut types = Types::new();
        let target = target("x86_64-unknown-linux-gnu");
        let int = types.int(IntKind::Int);
        let id = record(&mut types, &target, &[int, int, int]);
        let plan = plan(&types, &target, Convention::Target, types.void(), &[id], &[], false)
            .expect("a plan");
        // Twelve bytes in two registers, the second holding the four that are left, and the
        // load that reads them is four bytes wide and not eight.
        assert_eq!(plan.args[0].types, vec![Type::int(64), Type::int(32)]);
        assert_eq!(plan.args[0].size, 12);
        assert_eq!(plan.args[0].reach(), 12);
    }

    #[test]
    fn a_register_wider_than_what_is_left_of_the_object_is_what_a_buffer_is_for() {
        let mut types = Types::new();
        let target = target("x86_64-unknown-linux-gnu");
        let char_ty = types.int(IntKind::Char);
        let array = types.array(char_ty, ArrayLen::Fixed(5));
        let id = record(&mut types, &target, &[array]);
        let plan = plan(&types, &target, Convention::Target, types.void(), &[id], &[], false)
            .expect("a plan");
        // Five bytes in one register, which is read eight bytes at a time, so the three bytes
        // past the object are what the walk has to go around.
        assert_eq!(plan.args[0].size, 5);
        assert_eq!(plan.args[0].reach(), 8);
    }

    /// Darwin arm64 is the one target on the table where the answer for an argument past the
    /// `...` is not the answer for the same argument in front of it. Two floats are a homogeneous
    /// aggregate and go in two vector registers as a fixed argument, and the same two floats go in
    /// the argument area when they are variadic. A caller that asks the fixed question for a
    /// variadic argument writes registers the callee never reads, and `va_arg` then returns
    /// whatever was on the stack, which is why this is a test and not a comment.
    /// Apple's arm64 extends a `char` or a `short` to 32 bits by its own sign on both sides of a
    /// call, which clang relies on: its callee of `f(unsigned char c)` returning `c` is a bare
    /// `ret`. AAPCS64 promises nothing about those bits and nothing is marked there.
    #[test]
    fn a_narrow_integer_travels_extended_on_darwin_arm64_and_as_itself_elsewhere() {
        let mut types = Types::new();
        let darwin = target("aarch64-apple-darwin");
        let (schar, uchar) = (types.int(IntKind::SChar), types.int(IntKind::UChar));
        let (short, ushort) = (types.int(IntKind::Short), types.int(IntKind::UShort));
        let (char_ty, int, boolean) =
            (types.int(IntKind::Char), types.int(IntKind::Int), types.boolean());
        let params = [schar, uchar, short, ushort, char_ty, int, boolean];
        let narrow =
            plan(&types, &darwin, Convention::Target, uchar, &params, &[], false).expect("a plan");
        let abis: Vec<Abi> = narrow.signature.params.iter().map(|param| param.abi).collect();
        // A plain `char` is signed there, the same as on every other Apple target.
        let (s, z, p) = (Abi::Sext, Abi::Zext, Abi::Plain);
        assert_eq!(abis, vec![s, z, s, z, s, p, z]);
        assert_eq!(narrow.signature.returns[0].abi, Abi::Zext);

        // A one byte structure travels in a register too, and has no sign to extend by.
        let small = record(&mut types, &darwin, &[uchar]);
        let bytes =
            plan(&types, &darwin, Convention::Target, small, &[small], &[], false).expect("a plan");
        assert_eq!(bytes.signature.params[0].abi, Abi::Plain);
        assert_eq!(bytes.signature.returns[0].abi, Abi::Plain);

        let linux = target("aarch64-unknown-linux-gnu");
        let elsewhere =
            plan(&types, &linux, Convention::Target, uchar, &params, &[], false).expect("a plan");
        assert!(elsewhere.signature.params.iter().all(|param| param.abi == Abi::Plain));
        assert_eq!(elsewhere.signature.returns[0].abi, Abi::Plain);
    }

    #[test]
    fn an_argument_past_the_dots_asks_a_different_question_on_darwin_arm64() {
        let mut types = Types::new();
        let target = target("aarch64-apple-darwin");
        let float = types.float(FloatKind::Float);
        let int = types.int(IntKind::Int);
        let pair = record(&mut types, &target, &[float, float]);

        let fixed = plan(&types, &target, Convention::Target, int, &[pair], &[pair], false)
            .expect("a plan");
        assert_eq!(fixed.args[0].types, vec![Type::float(Float::F32), Type::float(Float::F32)]);

        // The same record, one place further along a `...`, and the caller copies the bytes.
        let variadic = plan(&types, &target, Convention::Target, int, &[int], &[int, pair], true)
            .expect("a plan");
        assert_eq!(variadic.args[1].pass, Pass::Memory);
        assert_eq!(
            variadic.varargs,
            vec![Abi::ByVal { size: 8, align: 4, drains: Drains::Nothing }]
        );
    }

    /// The three ABIs that are not Darwin arm64 answer the two questions the same way, and a
    /// change to the classifier that made them differ would be a change to every `printf` on
    /// three targets. Windows and RISC-V are here beside SysV because the flag they carry,
    /// `Variadic::BothBanks` for one and `SameAsFixed` for the others, is a fact for the backend
    /// rather than for the form the value travels in.
    #[test]
    fn everywhere_else_the_two_questions_have_the_same_answer() {
        for triple in
            ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc", "riscv64-unknown-linux-gnu"]
        {
            let mut types = Types::new();
            let target = target(triple);
            let float = types.float(FloatKind::Float);
            let int = types.int(IntKind::Int);
            let pair = record(&mut types, &target, &[float, float]);

            let fixed =
                plan(&types, &target, Convention::Target, int, &[int, pair], &[int, pair], false)
                    .expect("a plan for a fixed argument");
            let variadic =
                plan(&types, &target, Convention::Target, int, &[int], &[int, pair], true)
                    .expect("a plan for a variadic one");
            assert_eq!(fixed.args[1].pass, variadic.args[1].pass, "{triple}");
            assert_eq!(fixed.args[1].types, variadic.args[1].types, "{triple}");
        }
    }

    /// `long double twice(long double);` on mingw, where both the argument and the return value
    /// are addresses and the signature the IR writes has two pointers in it and returns nothing.
    ///
    /// The type is the one the environment settles: it is sixteen bytes of x87 under mingw and a
    /// `double` under MSVC, so the same declaration is two pointers on one Windows target and two
    /// `double`s on the other.
    #[test]
    fn a_wide_scalar_on_windows_travels_as_an_address_both_ways() {
        let types = Types::new();
        let mingw = target("x86_64-pc-windows-gnu");
        let msvc = target("x86_64-pc-windows-msvc");
        let wide = types.float(FloatKind::LongDouble);
        let both =
            plan(&types, &mingw, Convention::Target, wide, &[wide], &[], false).expect("a plan");
        assert!(both.returns_through_memory());
        assert_eq!(both.ret.pass, Pass::Reference);
        assert_eq!(both.args[0].pass, Pass::Reference);
        assert_eq!(both.args[0].types, vec![Type::PTR]);
        assert!(both.signature.returns.is_empty());
        assert_eq!(both.signature.params.len(), 2);
        assert_eq!(both.signature.params[0].abi, Abi::Sret { size: 16, align: 16, popped: None });

        let narrow =
            plan(&types, &msvc, Convention::Target, wide, &[wide], &[], false).expect("a plan");
        assert_eq!(narrow.args[0].pass, Pass::Direct);
        assert_eq!(narrow.args[0].types, vec![Type::float(Float::F64)]);
        assert!(!narrow.returns_through_memory());
    }

    #[test]
    fn the_same_declaration_travels_differently_on_two_targets() {
        let mut types = Types::new();
        let linux = target("x86_64-unknown-linux-gnu");
        let windows = target("x86_64-pc-windows-msvc");
        let long = types.int(IntKind::Long);
        let id = record(&mut types, &linux, &[long, long]);
        let sysv = plan(&types, &linux, Convention::Target, types.void(), &[id], &[], false)
            .expect("a plan");
        let win64 = plan(&types, &windows, Convention::Target, types.void(), &[id], &[], false)
            .expect("a plan");
        assert_eq!(sysv.args[0].types, vec![Type::int(64), Type::int(64)]);
        assert_eq!(win64.args[0].pass, Pass::Reference);
        assert_eq!(win64.args[0].types, vec![Type::PTR]);
    }

    /// A function of the other convention on each platform travels the way the other platform
    /// has it, with the types the platform it is on gives it. On Linux under `ms_abi` a sixteen
    /// byte structure and a `long double` go by reference, since both are sixteen bytes and
    /// Windows passes only the sizes of an integer by value; the `long double` is still the x87
    /// format, which is what gcc does. On Windows under `sysv_abi` the same structure goes in two
    /// registers. And the convention is on the signature for everything below to read.
    #[test]
    fn a_function_of_the_other_convention_travels_the_other_platform_s_way() {
        let mut types = Types::new();
        let linux = target("x86_64-unknown-linux-gnu");
        let mingw = target("x86_64-pc-windows-gnu");
        let long = types.int(IntKind::Long);
        let pair = record(&mut types, &linux, &[long, long]);
        let wide = types.float(FloatKind::LongDouble);
        let void = types.void();

        let ms =
            plan(&types, &linux, Convention::Ms, wide, &[pair, wide], &[], false).expect("a plan");
        assert_eq!(ms.signature.convention, Convention::Ms);
        assert!(ms.returns_through_memory(), "a long double comes back through memory");
        assert_eq!(ms.args[0].pass, Pass::Reference);
        assert_eq!(ms.args[1].pass, Pass::Reference);
        assert_eq!(ms.signature.params[0].abi, Abi::Sret { size: 16, align: 16, popped: None });

        let sysv =
            plan(&types, &mingw, Convention::Sysv, void, &[pair], &[], false).expect("a plan");
        assert_eq!(sysv.signature.convention, Convention::Sysv);
        assert_eq!(sysv.args[0].types, vec![Type::int(64), Type::int(64)]);

        let native =
            plan(&types, &linux, Convention::Target, void, &[pair], &[], false).expect("a plan");
        assert_eq!(native.signature.convention, Convention::Target);
        assert_eq!(native.args[0].types, vec![Type::int(64), Type::int(64)]);

        // AArch64 has no second convention, so asking for one is a plan nobody can make.
        let arm = target("aarch64-unknown-linux-gnu");
        assert!(plan(&types, &arm, Convention::Ms, void, &[], &[], false).is_err());
    }

    /// `va_arg(ap, __int128)` reads two general purpose registers on System V, which is what makes
    /// the walk over the list want room for both at once, and on Windows the argument is the
    /// address of a copy, which the walk there works out from the size and needs no slots for.
    #[test]
    fn an_int128_read_off_a_list_is_two_words_on_sysv_and_none_on_windows() {
        let types = Types::new();
        let wide = types.int(IntKind::Int128);
        let half = Slot::Integer { offset: 0, size: 8 };
        let sysv = va_slots(&types, &target("x86_64-unknown-linux-gnu"), wide);
        assert_eq!(sysv, vec![half, Slot::Integer { offset: 8, size: 8 }]);
        assert!(va_slots(&types, &target("x86_64-pc-windows-gnu"), wide).is_empty());
    }

    /// On wasm32 a structure whose one member is an `__int128` travels as the member, which is
    /// one `i128` that the backend splits into two `i64` values and returns through memory. That
    /// is clang's signature. Before, the slot was cut to an `i64` and the high half was lost.
    #[test]
    fn a_structure_of_one_int128_travels_on_wasm32_as_the_int128() {
        let mut types = Types::new();
        let wasm = target("wasm32-unknown-wasip1");
        let wide = types.int(IntKind::Int128);
        let one = record(&mut types, &wasm, &[wide]);
        let bare =
            plan(&types, &wasm, Convention::Target, wide, &[wide], &[], false).expect("a plan");
        let wrapped =
            plan(&types, &wasm, Convention::Target, one, &[one], &[], false).expect("a plan");
        assert_eq!(wrapped.args[0].types, vec![Type::int(128)]);
        assert_eq!(wrapped.args[0].types, bare.args[0].types);
        assert_eq!(wrapped.signature.params, bare.signature.params);
        assert_eq!(wrapped.signature.returns, bare.signature.returns);
    }
}
