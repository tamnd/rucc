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

use rucc_ir::{Abi, Float, Param, Signature, Type};
use rucc_target::{Arg, Call, Kind, Pass, Piece, Scalar, Shape, Slot, TargetInfo};
use rucc_types::{ArrayLen, TypeId, TypeKind, Types, float_format, layout};

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
    },
    /// Something the classification has no vocabulary for, which today is a GNU vector.
    ///
    /// A vector is not a scalar and not an aggregate as far as [`Arg`] is concerned, so it is
    /// not asked about and travels as the value it is, which is what the walk did with one
    /// before any of this was here.
    Opaque(Type),
}

impl Shaped {
    /// What the target is asked about.
    fn arg(&self) -> Option<Arg<'_>> {
        match self {
            Self::Void => Some(Arg::Void),
            Self::Scalar(scalar) => Some(Arg::Scalar(*scalar)),
            Self::Aggregate { size, align, pieces, complex } => Some(Arg::Aggregate(Shape {
                size: *size,
                align: *align,
                pieces,
                complex: *complex,
            })),
            Self::Opaque(_) => None,
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
            // A vector travels as itself and nothing copies it, so neither of these is read.
            Self::Opaque(_) => (0, 1),
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

/// Classifies one call and builds the IR signature for it.
///
/// `params` are the parameters the callee's type names and `actual` is what a call site passes,
/// which is longer than `params` for a variadic call and is empty for a definition, where the
/// question is only about the parameters. The error is what to report, which is a message rather
/// than a kind because there is exactly one thing every caller does with it.
pub(crate) fn plan(
    types: &Types,
    target: &TargetInfo,
    ret: TypeId,
    params: &[TypeId],
    actual: &[TypeId],
    variadic: bool,
) -> Result<Plan, &'static str> {
    let mut call = target.call();
    let shaped = shape(types, target, ret).ok_or("returning a value of this type")?;
    let ret = travel(types, target, &mut call, &shaped, true, ret);

    let mut signature = Signature::new();
    signature.variadic = variadic;
    if matches!(ret.pass, Pass::Reference | Pass::Memory) {
        let (size, align) = (ret.size, ret.align);
        signature.params.push(Param::with_abi(Type::PTR, Abi::Sret { size, align }));
    } else {
        signature.returns.extend(ret.types.iter().map(|ty| Param::new(*ty)));
    }

    let count = params.len().max(actual.len());
    let mut args = Vec::with_capacity(count);
    let mut varargs = Vec::new();
    for index in 0..count {
        let ty = *params.get(index).or_else(|| actual.get(index)).expect("one of the two");
        let shaped = shape(types, target, ty).ok_or("passing a value of this type")?;
        let travel = travel(types, target, &mut call, &shaped, false, ty);
        if index < params.len() {
            signature.params.extend(travel.types.iter().map(|ty| param(&travel, *ty)));
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
        Pass::Memory => Param::with_abi(ty, Abi::ByVal { size: travel.size, align: travel.align }),
        _ => Param::new(ty),
    }
}

/// Asks the target about one value and works out what the IR needs to say it.
fn travel(
    types: &Types,
    target: &TargetInfo,
    call: &mut Call,
    shaped: &Shaped,
    returning: bool,
    ty: TypeId,
) -> Travel {
    let (size, align) = shaped.extent();
    let Some(arg) = shaped.arg() else {
        // A vector, which the classification has nothing to say about. It travels as itself and
        // spends nothing, which is what the walk did with one before this file existed.
        let Shaped::Opaque(value) = shaped else { unreachable!("every other shape is an arg") };
        return Travel { pass: Pass::Direct, size, align, types: vec![*value] };
    };
    let pass = if returning { call.returns(&arg) } else { call.argument(&arg) };
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
    Travel { pass, size, align, types }
}

/// The IR type one register's worth of an object is read as.
///
/// A slot as wide as a register is that register's integer type. One that is not, which is what
/// the last eightbyte of a twelve byte structure is, is rounded up to the next width a machine
/// has an instruction for, and the walk is what keeps the load from reading past the object.
pub(crate) fn slot_type(slot: Slot) -> Type {
    match slot {
        Slot::Integer { size, .. } => Type::int(size.next_power_of_two().clamp(1, 8) * 8),
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
    u64::from(slot_type(slot).bits().div_ceil(8))
}

/// Flattens a C type into what an ABI reads, and [`None`] for one it cannot describe.
pub(crate) fn shape(types: &Types, target: &TargetInfo, ty: TypeId) -> Option<Shaped> {
    let id = types.canonical(ty);
    if matches!(types.kind(id), TypeKind::Void) {
        return Some(Shaped::Void);
    }
    if let Some(scalar) = scalar(types, target, id) {
        return Some(Shaped::Scalar(scalar));
    }
    if matches!(types.kind(id), TypeKind::Vector { .. }) {
        return Some(Shaped::Opaque(repr::value_type(types, target, id)?));
    }
    let size = repr::size_of(types, target, id);
    let align = u64::from(repr::align_of(types, target, id));
    let mut flatten = Flatten { types, target, pieces: Vec::new(), capped: size > IN_REGISTERS };
    flatten.push(id, 0)?;
    let mut pieces = flatten.pieces;
    // Offset order is what every rule is written over, and a union is what puts two members at
    // one offset. Two members that are the same thing in the same place are one piece, which is
    // what makes a union of two `float`s the homogeneous aggregate AAPCS64 says it is.
    pieces.sort_by_key(|piece| piece.offset);
    pieces.dedup();
    let complex = matches!(types.kind(id), TypeKind::Complex(_));
    Some(Shaped::Aggregate { size, align, pieces, complex })
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

    /// Everything in one type, at its offset from the start of the object.
    fn push(&mut self, ty: TypeId, at: u64) -> Option<()> {
        if self.full() {
            return Some(());
        }
        let id = self.types.canonical(ty);
        if let Some(scalar) = scalar(self.types, self.target, id) {
            self.pieces.push(Piece { offset: at, scalar });
            return Some(());
        }
        match self.types.kind(id) {
            TypeKind::Complex(kind) => {
                let format = float_format(kind, self.target);
                let size = repr::size_of(self.types, self.target, id) / 2;
                let scalar = Scalar { kind: Kind::Float(format), size, align: size };
                self.pieces.push(Piece { offset: at, scalar });
                self.pieces.push(Piece { offset: at + size, scalar });
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
        let plan = plan(&types, &target, types.void(), &[id], &[], false).expect("a plan");
        assert_eq!(plan.args[0].pass, Pass::Memory);
    }

    #[test]
    fn a_structure_that_travels_in_registers_says_which_bytes_each_one_holds() {
        let mut types = Types::new();
        let target = target("x86_64-unknown-linux-gnu");
        let int = types.int(IntKind::Int);
        let double = types.float(FloatKind::Double);
        let id = record(&mut types, &target, &[int, double]);
        let plan = plan(&types, &target, id, &[id], &[], false).expect("a plan");
        // Sixteen bytes, an `int` in the first eightbyte and a `double` in the second, which is
        // one general purpose register and one vector register both going in and coming back.
        assert_eq!(plan.args[0].types, vec![Type::int(64), Type::float(Float::F64)]);
        assert_eq!(plan.args[0].slots()[1].offset(), 8);
        assert_eq!(plan.ret.types, vec![Type::int(64), Type::float(Float::F64)]);
        assert!(!plan.returns_through_memory());
        assert_eq!(plan.signature.params.len(), 2);
    }

    #[test]
    fn a_return_value_too_large_for_the_registers_becomes_the_first_parameter() {
        let mut types = Types::new();
        let target = target("x86_64-unknown-linux-gnu");
        let double = types.float(FloatKind::Double);
        let id = record(&mut types, &target, &[double, double, double]);
        let plan = plan(&types, &target, id, &[], &[], false).expect("a plan");
        assert!(plan.returns_through_memory());
        assert!(plan.signature.returns.is_empty());
        assert_eq!(plan.signature.params.len(), 1);
        assert_eq!(plan.signature.params[0].abi, Abi::Sret { size: 24, align: 8 });
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
        let plan = plan(&types, &target, int, &[ptr], &[ptr, int, big], true).expect("a plan");
        assert_eq!(plan.signature.params.len(), 1);
        assert_eq!(plan.args[2].pass, Pass::Memory);
        assert_eq!(plan.varargs, vec![Abi::Plain, Abi::ByVal { size: 24, align: 8 }]);
    }

    #[test]
    fn a_call_whose_arguments_all_travel_as_themselves_says_nothing_about_them() {
        let mut types = Types::new();
        let target = target("x86_64-unknown-linux-gnu");
        let int = types.int(IntKind::Int);
        let ptr = types.pointer(types.int(IntKind::Char));
        let plan = plan(&types, &target, int, &[ptr], &[ptr, int, int], true).expect("a plan");
        assert!(plan.varargs.is_empty());
    }

    #[test]
    fn what_a_structure_reaches_into_is_not_always_what_it_is() {
        let mut types = Types::new();
        let target = target("x86_64-unknown-linux-gnu");
        let int = types.int(IntKind::Int);
        let id = record(&mut types, &target, &[int, int, int]);
        let plan = plan(&types, &target, types.void(), &[id], &[], false).expect("a plan");
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
        let plan = plan(&types, &target, types.void(), &[id], &[], false).expect("a plan");
        // Five bytes in one register, which is read eight bytes at a time, so the three bytes
        // past the object are what the walk has to go around.
        assert_eq!(plan.args[0].size, 5);
        assert_eq!(plan.args[0].reach(), 8);
    }

    #[test]
    fn the_same_declaration_travels_differently_on_two_targets() {
        let mut types = Types::new();
        let linux = target("x86_64-unknown-linux-gnu");
        let windows = target("x86_64-pc-windows-msvc");
        let long = types.int(IntKind::Long);
        let id = record(&mut types, &linux, &[long, long]);
        let sysv = plan(&types, &linux, types.void(), &[id], &[], false).expect("a plan");
        let win64 = plan(&types, &windows, types.void(), &[id], &[], false).expect("a plan");
        assert_eq!(sysv.args[0].types, vec![Type::int(64), Type::int(64)]);
        assert_eq!(win64.args[0].pass, Pass::Reference);
        assert_eq!(win64.args[0].types, vec![Type::PTR]);
    }
}
