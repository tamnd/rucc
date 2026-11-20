//! Which type each byte of a record has, and how often one granule of them holds only one type.
//!
//! Design: `spec/safe-memory/05-representation.md` sections 5.2.3 and 5.2.5, and this is the
//! measurement `spec/safe-memory/17-open-questions.md` question 6 asks for.
//!
//! The question it answers, stated exactly. A type plane that records an effective type per
//! byte costs four bytes for every byte of the program, which is the 4:1 that makes TySan
//! unaffordable. The compression in document 05 stores one entry per granule of `g` bytes and
//! falls back to a per-byte side table over the whole granule for the ones whose bytes do not
//! all agree, which costs `4/g + 4h` bytes per byte where `h` is the fraction of granules that
//! disagree. Tier D's memory budget allows 1.25. Everything the type plane costs depends on
//! where `h` really lands, which had never been measured until this ran.
//!
//! What it found is why [`GRANULE`] is eight and not the sixteen document 05 first assumed.
//! On a 64-bit target the unit of a distinct type is eight bytes, so a sixteen byte granule
//! holds two of them and `struct { char *p; int a; int b; }` is enough to make it disagree.
//! SQLite is 64.8% heterogeneous at sixteen, which costs 2.84 against a budget of 1.25, and
//! 12.6% at eight, which costs 1.00.
//!
//! Working from the compiler's own layouts rather than from DWARF, which is what question 6
//! proposes, because they are the same layouts: DWARF is where these numbers go, not where
//! they come from. Reading them here also keeps the type identity, which in DWARF is a
//! reference to resolve rather than a fact to hand.
//!
//! What counts as one type is a decision and not a discovery, so it is [`Keying`] and both
//! answers get reported.

use rucc_target::TargetInfo;

use crate::kind::{ArrayLen, RecordId, RecordKind, TypeKind};
use crate::layout::layout;
use crate::record::Field;
use crate::types::{TypeId, Types};

/// How many bytes one granule covers.
///
/// Eight, because that is where the curve in document 05.2.5 bottoms out on both of the inputs
/// measured, and because it is the size of a pointer, which is the thing whose type the plane
/// most wants to be right about. It is a default and not a law, which is why [`measure`] takes
/// the size rather than reading it: the granule size is the one dial the design has, and what
/// it buys is the thing worth reporting.
pub const GRANULE: u64 = 8;

/// The granule sizes the report walks.
///
/// One is byte granularity, which is the uncompressed plane and is here as the number the
/// compression has to beat.
const SIZES: &[u64] = &[1, 4, 8, 16, 32, 64];

/// The largest record this measures, in bytes.
///
/// Painting a byte at a time means a record with a large array member costs its own size in
/// memory, and a translation unit is allowed to declare a structure holding a megabyte of
/// buffer. Skipped records are counted and reported rather than silently dropped, because a
/// measurement that quietly ignores its largest inputs is the one that reads best.
const LIMIT: u64 = 1 << 20;

/// How many layouts of one record this is willing to walk.
///
/// A union is a choice rather than a coexistence: at any moment its bytes hold whichever
/// member was last stored, so a union of a `long` and a `double` fills its granule with one
/// type either way and a plane keyed by granule has no trouble with it. That means a record
/// containing unions has one layout per combination of choices, and a record with several
/// unions has the product of them. Past this many the record is measured with every member
/// painted at once instead, which can only say a granule disagrees when it might not, so the
/// answer stays on the pessimistic side of the truth.
const LAYOUTS: usize = 32;

/// What is treated as one type when deciding whether a granule agrees with itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keying {
    /// Two bytes agree when their types are the same after typedefs and qualifiers are
    /// resolved and an enumeration is replaced by what it is represented in.
    ///
    /// Qualifiers go because an effective type has none: writing through a `const int *` and
    /// through an `int *` stores the same effective type, and 6.5p6 says so. Enumerations go
    /// because an enumeration is compatible with an implementation-defined integer type and
    /// keeping them apart would count a real structure as mixed over a distinction no access
    /// can observe.
    Exact,
    /// The same, and additionally every pointer type is one type.
    ///
    /// Worth reporting separately because it is the one classification choice with a large
    /// effect on the answer and no obviously right side. A plane that distinguishes `char *`
    /// from `struct Foo *` catches a type confusion between them; a plane that does not is
    /// cheaper, and how much cheaper is what the two numbers say.
    PointersTogether,
}

/// What one byte holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cell {
    /// Padding, or a byte no member reaches. It has no effective type and the plane stores
    /// nothing for it.
    Pad,
    /// Exactly one type reaches this byte.
    One(TypeKind),
    /// More than one does, which within one layout means overlapping bit-fields of different
    /// declared types.
    Mixed,
}

impl Cell {
    /// The result of a member of type `kind` also reaching a byte that already holds `self`.
    fn with(self, kind: TypeKind) -> Cell {
        match self {
            Cell::Pad => Cell::One(kind),
            Cell::One(had) if had == kind => self,
            _ => Cell::Mixed,
        }
    }
}

/// How the bytes of some records fall into granules.
///
/// Everything here counts, so two tallies add, and the whole translation unit is the sum of
/// its records.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    /// How many records were measured.
    pub records: u64,
    /// How many were too large to measure and were skipped.
    pub skipped: u64,
    /// How many bytes those records occupy in total.
    pub bytes: u64,
    /// How many of those bytes have no effective type, meaning padding.
    pub padding: u64,
    /// Granules whose typed bytes all agree, which cost one entry.
    pub uniform: u64,
    /// Granules whose typed bytes do not, which cost an entry and a sixteen byte side table.
    pub mixed: u64,
    /// Granules with no typed bytes at all, which are padding to the end of the record.
    pub blank: u64,
}

impl Tally {
    /// How many granules were measured.
    #[must_use]
    pub fn granules(&self) -> u64 {
        self.uniform + self.mixed + self.blank
    }

    /// The fraction of granules that need a side table, which is question 6's `h`.
    ///
    /// A blank granule needs no side table, so it counts as agreeing here even though nothing
    /// in it agrees about anything. Zero granules gives zero, because a translation unit that
    /// declares no records puts no pressure on the budget.
    #[must_use]
    pub fn disagreeing(&self) -> f64 {
        let all = self.granules();
        if all == 0 { 0.0 } else { self.mixed as f64 / all as f64 }
    }

    /// Bytes of type plane per byte of program, under document 05's compression.
    ///
    /// One four byte entry per granule, plus a four byte per-byte side table over the whole
    /// granule for the ones that need one, so it is `4/g + 4h` and the granule size only
    /// touches the first term. Tier D's budget is 1.25 and the uncompressed plane is 4, which
    /// is also what this returns at a granule of one byte, since nothing disagrees with itself.
    #[must_use]
    pub fn ratio(&self, granule: u64) -> f64 {
        4.0 / granule as f64 + 4.0 * self.disagreeing()
    }

    /// Adds another tally into this one.
    pub fn absorb(&mut self, other: Tally) {
        self.records += other.records;
        self.skipped += other.skipped;
        self.bytes += other.bytes;
        self.padding += other.padding;
        self.uniform += other.uniform;
        self.mixed += other.mixed;
        self.blank += other.blank;
    }
}

/// Measures one record.
///
/// Returns nothing for a record that is not complete, since an incomplete one has no layout
/// and no members to walk. A record too large to paint comes back as a tally that counts
/// only its skip.
#[must_use]
pub fn measure(
    types: &Types,
    id: RecordId,
    target: &TargetInfo,
    keying: Keying,
    granule: u64,
) -> Option<Tally> {
    let info = types.record_info(id);
    let size = info.layout?.size;
    if size > LIMIT {
        return Some(Tally { skipped: 1, ..Tally::default() });
    }
    let width = usize::try_from(granule).ok().filter(|width| *width > 0)?;
    let mut layouts = vec![vec![Cell::Pad; usize::try_from(size).ok()?]];
    paint_record(types, id, 0, target, keying, &mut layouts);

    let mut tally = Tally { records: 1, bytes: size, ..Tally::default() };
    let count = layouts[0].len().div_ceil(width);
    for index in 0..count {
        let from = index * width;
        let to = (from + width).min(layouts[0].len());
        // A granule disagrees if it disagrees under any one layout, and is padding only if it
        // is padding under all of them, which is why the whole granule is looked at once per
        // layout rather than a byte at a time across them.
        let mut disagrees = false;
        let mut typed = false;
        for layout in &layouts {
            let (mixed, seen) = verdict(&layout[from..to]);
            disagrees |= mixed;
            typed |= seen;
        }
        match (disagrees, typed) {
            (true, _) => tally.mixed += 1,
            (false, true) => tally.uniform += 1,
            (false, false) => tally.blank += 1,
        }
        tally.padding += (from..to)
            .filter(|byte| layouts.iter().all(|layout| layout[*byte] == Cell::Pad))
            .count() as u64;
    }
    Some(tally)
}

/// Whether the bytes of one granule under one layout disagree, and whether any is typed.
fn verdict(granule: &[Cell]) -> (bool, bool) {
    let mut seen: Option<TypeKind> = None;
    for cell in granule {
        match *cell {
            Cell::Pad => {}
            Cell::Mixed => return (true, true),
            Cell::One(kind) => match seen {
                None => seen = Some(kind),
                Some(had) if had == kind => {}
                Some(_) => return (true, true),
            },
        }
    }
    (false, seen.is_some())
}

/// Measures every complete record in the translation unit.
#[must_use]
pub fn measure_all(types: &Types, target: &TargetInfo, keying: Keying, granule: u64) -> Tally {
    let mut tally = Tally::default();
    for (id, _) in types.records() {
        if let Some(one) = measure(types, id, target, keying, granule) {
            tally.absorb(one);
        }
    }
    tally
}

/// The measurement for one translation unit, as text.
///
/// Text and not JSON, unlike the safety summary, because the safety summary is a number a
/// build watches and this is a table somebody reads once and writes a paragraph about. One
/// line per record so the worst offenders can be found with `sort`, then the curve of what the
/// plane costs against the granule size, under both keyings.
///
/// # Panics
///
/// Panics if writing to a `String` fails, which it does not.
#[must_use]
pub fn report(types: &Types, names: &rucc_base::Interner, target: &TargetInfo) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    writeln!(out, "per record, at a granule of {GRANULE} bytes\n").expect("a string takes writes");
    writeln!(out, "{:>8} {:>8} {:>8} {:>8}  record", "bytes", "uniform", "mixed", "blank")
        .expect("a string takes writes");
    for (id, info) in types.records() {
        let Some(tally) = measure(types, id, target, Keying::Exact, GRANULE) else {
            continue;
        };
        let kind = match info.kind {
            RecordKind::Struct => "struct",
            RecordKind::Union => "union",
        };
        let tag = match info.tag {
            Some(tag) => names.resolve(tag).to_string(),
            None => format!("<anonymous {}>", id.0),
        };
        writeln!(
            out,
            "{:>8} {:>8} {:>8} {:>8}  {kind} {tag}",
            tally.bytes, tally.uniform, tally.mixed, tally.blank
        )
        .expect("a string takes writes");
    }
    for keying in [Keying::Exact, Keying::PointersTogether] {
        let label = match keying {
            Keying::Exact => "every type distinct",
            Keying::PointersTogether => "every pointer one type",
        };
        writeln!(out, "\n{label}").expect("a string takes writes");
        writeln!(
            out,
            "{:>8} {:>8} {:>9} {:>9} {:>9} {:>9}",
            "granule", "records", "bytes", "granules", "disagree", "plane"
        )
        .expect("a string takes writes");
        for &size in SIZES {
            let tally = measure_all(types, target, keying, size);
            writeln!(
                out,
                "{:>8} {:>8} {:>9} {:>9} {:>9.4} {:>9.4}",
                size,
                tally.records,
                tally.bytes,
                tally.granules(),
                tally.disagreeing(),
                tally.ratio(size)
            )
            .expect("a string takes writes");
        }
    }
    let whole = measure_all(types, target, Keying::Exact, GRANULE);
    writeln!(out, "\npadding   {} of {} bytes", whole.padding, whole.bytes)
        .expect("a string takes writes");
    writeln!(out, "skipped   {} records too large to measure", whole.skipped)
        .expect("a string takes writes");
    writeln!(out, "budget    1.25 bytes of plane per byte of program, at Tier D")
        .expect("a string takes writes");
    out
}

/// Writes the type of every byte `ty` occupies at `base` into every layout.
///
/// Recursion is what makes the answer right: the effective type a store leaves behind is the
/// type of the lvalue it stored through, which for a nested structure is the scalar member and
/// not the structure. Padding is never written, so it stays [`Cell::Pad`] and the plane owes
/// it nothing.
fn paint(
    types: &Types,
    ty: TypeId,
    base: u64,
    target: &TargetInfo,
    keying: Keying,
    layouts: &mut Vec<Vec<Cell>>,
) {
    let canonical = types.canonical(ty);
    match types.kind(canonical) {
        TypeKind::Record(id) => paint_record(types, id, base, target, keying, layouts),
        TypeKind::Array { elem, len } => {
            let ArrayLen::Fixed(count) = len else {
                // A flexible array member, a variable length array or `[*]`. None of them has
                // a size the declaration knows, and a flexible array member deliberately
                // contributes nothing to `sizeof`, so there are no bytes here to paint.
                return;
            };
            let Ok(each) = layout(types, elem, target) else {
                return;
            };
            for index in 0..count {
                let Some(at) = each.size.checked_mul(index).and_then(|off| base.checked_add(off))
                else {
                    return;
                };
                paint(types, elem, at, target, keying, layouts);
            }
        }
        // An atomic type is its inner type with a rule about how it is accessed, and the plane
        // records what was stored rather than how.
        TypeKind::Atomic(inner) => paint(types, inner, base, target, keying, layouts),
        _ => {
            let Ok(whole) = layout(types, canonical, target) else {
                return;
            };
            fill(types, canonical, base, whole.size, keying, layouts);
        }
    }
}

/// Writes the type of every byte one record occupies at `base` into every layout.
///
/// A `struct` places its members side by side, so every one of them goes into every layout.
/// A `union` places them on top of each other and the program picks one, so it multiplies the
/// layouts instead: the answer for a union of a `long` and a `double` is that its granule holds
/// one type, whichever member was stored, and a plane keyed by granule handles it. That is the
/// difference between a choice and a coexistence, and getting it wrong is what would make every
/// tagged value in a real program look like it needs a side table.
///
/// Separate from [`paint`] because a record is reached both through a type and through a
/// [`RecordId`] on its own, and asking the type table for the type of a record it already has
/// would need to intern one.
fn paint_record(
    types: &Types,
    id: RecordId,
    base: u64,
    target: &TargetInfo,
    keying: Keying,
    layouts: &mut Vec<Vec<Cell>>,
) {
    let info = types.record_info(id);
    let grown = match info.kind {
        RecordKind::Union if info.fields.len() > 1 => {
            layouts.len().checked_mul(info.fields.len()).filter(|grown| *grown <= LAYOUTS)
        }
        _ => None,
    };
    if let Some(grown) = grown {
        let start = layouts.clone();
        let mut out = Vec::with_capacity(grown);
        for field in &info.fields {
            let mut copy = start.clone();
            place(types, field, base, target, keying, &mut copy);
            out.append(&mut copy);
        }
        *layouts = out;
        return;
    }
    for field in &info.fields {
        let at = match info.kind {
            RecordKind::Struct => base + field.offset,
            RecordKind::Union => base,
        };
        place(types, field, at, target, keying, layouts);
    }
}

/// Writes one member, placed at `at`, into every layout.
fn place(
    types: &Types,
    field: &Field,
    at: u64,
    target: &TargetInfo,
    keying: Keying,
    layouts: &mut Vec<Vec<Cell>>,
) {
    match field.bits {
        // A zero width bit-field places nothing and is only there to move the next member
        // along.
        Some(0) => {}
        // A bit-field has no address, so the granule question is about the bytes a load of it
        // would touch. Two bit-fields of different declared types sharing a byte make that byte
        // disagree with itself, which is the honest answer: a plane keyed by byte cannot tell
        // them apart, and unlike a union they are both there at once.
        Some(width) => {
            let bytes = u64::from(field.bit + width).div_ceil(8);
            fill(types, field.ty, at, bytes, keying, layouts);
        }
        None => paint(types, field.ty, at, target, keying, layouts),
    }
}

/// Records that `count` bytes from `base` hold the type `ty`, in every layout.
///
/// Clipped to the record rather than checked, because a union member painted at the start of a
/// record it does not fill and an array whose arithmetic ran past the end both want the same
/// answer, which is to write what is inside and drop what is not.
fn fill(
    types: &Types,
    ty: TypeId,
    base: u64,
    count: u64,
    keying: Keying,
    layouts: &mut [Vec<Cell>],
) {
    let kind = key(types, ty, keying);
    let Ok(from) = usize::try_from(base) else {
        return;
    };
    for cells in layouts.iter_mut() {
        let to = usize::try_from(base.saturating_add(count)).unwrap_or(usize::MAX).min(cells.len());
        if from >= to {
            continue;
        }
        for cell in &mut cells[from..to] {
            *cell = cell.with(kind);
        }
    }
}

/// What `ty` counts as, under `keying`.
fn key(types: &Types, ty: TypeId, keying: Keying) -> TypeKind {
    let kind = types.kind(types.canonical(ty));
    match kind {
        // An enumeration is compatible with the integer type it is represented in, so a byte
        // written through one and a byte written through the other hold the same effective
        // type and a plane that separated them would be counting a distinction no access can
        // make. Before the underlying type is decided there is nothing to fold to.
        TypeKind::Enum(id) => match types.enum_info(id).underlying {
            Some(underlying) => types.kind(types.canonical(underlying)),
            None => kind,
        },
        TypeKind::Pointer(_) if keying == Keying::PointersTogether => {
            TypeKind::Pointer(types.void())
        }
        _ => kind,
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_target::Triple;

    use super::*;
    use crate::kind::IntKind;
    use crate::layout_record;
    use crate::record::{FieldDecl, RecordOptions};

    fn target() -> TargetInfo {
        TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().expect("a triple"))
    }

    /// Builds a struct from its members and measures it under both keyings.
    fn built(types: &mut Types, names: &mut Interner, members: &[(&str, TypeId)]) -> RecordId {
        let fields: Vec<FieldDecl> = members
            .iter()
            .map(|(name, ty)| FieldDecl::new(Some(names.intern(name)), *ty))
            .collect();
        let id = types.declare_record(RecordKind::Struct, None);
        let laid_out =
            layout_record(types, RecordKind::Struct, &fields, &RecordOptions::default(), &target())
                .expect("a record with a layout");
        types.complete_record(id, laid_out);
        id
    }

    #[test]
    fn a_granule_of_one_type_agrees_with_itself() {
        let mut types = Types::new();
        let mut names = Interner::new();
        let long = types.int(IntKind::Long);
        let id = built(&mut types, &mut names, &[("a", long), ("b", long)]);

        let tally = measure(&types, id, &target(), Keying::Exact, 16).expect("a complete record");
        assert_eq!(tally.bytes, 16);
        assert_eq!(tally.uniform, 1);
        assert_eq!(tally.mixed, 0);
        assert_eq!(tally.padding, 0);
    }

    #[test]
    fn two_types_in_one_granule_do_not() {
        let mut types = Types::new();
        let mut names = Interner::new();
        let long = types.int(IntKind::Long);
        let double = types.float(crate::kind::FloatKind::Double);
        let id = built(&mut types, &mut names, &[("a", long), ("b", double)]);

        let tally = measure(&types, id, &target(), Keying::Exact, 16).expect("a complete record");
        assert_eq!(tally.bytes, 16);
        assert_eq!(tally.mixed, 1);
        assert_eq!(tally.uniform, 0);
    }

    #[test]
    fn padding_has_no_type_and_costs_nothing() {
        let mut types = Types::new();
        let mut names = Interner::new();
        let ch = types.int(IntKind::Char);
        let id = built(&mut types, &mut names, &[("a", ch)]);

        // One byte of member and fifteen bytes of nothing, since the record is one byte long
        // and the granule is the rest of the way to sixteen.
        let tally = measure(&types, id, &target(), Keying::Exact, 16).expect("a complete record");
        assert_eq!(tally.bytes, 1);
        assert_eq!(tally.padding, 0);
        assert_eq!(tally.uniform, 1);
    }

    #[test]
    fn padding_between_members_is_counted_and_does_not_make_a_granule_disagree() {
        let mut types = Types::new();
        let mut names = Interner::new();
        let ch = types.int(IntKind::Char);
        let long = types.int(IntKind::Long);
        let inner = built(&mut types, &mut names, &[("c", ch)]);
        let inner = types.record(inner);
        let id = built(&mut types, &mut names, &[("a", ch), ("b", long), ("c", inner)]);

        // `char` at 0, seven bytes of padding, `long` at 8, `char` at 16. The first granule
        // holds a `char` and a `long` so it disagrees; the second holds one `char` and fifteen
        // bytes of padding, so it does not.
        let tally = measure(&types, id, &target(), Keying::Exact, 16).expect("a complete record");
        assert_eq!(tally.bytes, 24);
        assert_eq!(tally.padding, 7 + 7);
        assert_eq!(tally.mixed, 1);
        assert_eq!(tally.uniform, 1);
    }

    #[test]
    fn an_array_paints_every_element_and_stays_one_type() {
        let mut types = Types::new();
        let mut names = Interner::new();
        let int = types.int(IntKind::Int);
        let array = types.array(int, ArrayLen::Fixed(16));
        let id = built(&mut types, &mut names, &[("a", array)]);

        let tally = measure(&types, id, &target(), Keying::Exact, 16).expect("a complete record");
        assert_eq!(tally.bytes, 64);
        assert_eq!(tally.uniform, 4);
        assert_eq!(tally.mixed, 0);
    }

    #[test]
    fn a_union_is_a_choice_and_not_a_coexistence() {
        let mut types = Types::new();
        let mut names = Interner::new();
        let long = types.int(IntKind::Long);
        let double = types.float(crate::kind::FloatKind::Double);
        let fields = [FieldDecl::new(Some(names.intern("i")), long), FieldDecl::new(None, double)];
        let id = types.declare_record(RecordKind::Union, None);
        let laid_out =
            layout_record(&types, RecordKind::Union, &fields, &RecordOptions::default(), &target())
                .expect("a union with a layout");
        types.complete_record(id, laid_out);

        // Eight bytes holding a `long` or eight bytes holding a `double`, never both, so the
        // granule holds one type either way and the plane needs no side table for it.
        let tally = measure(&types, id, &target(), Keying::Exact, 16).expect("a complete record");
        assert_eq!(tally.bytes, 8);
        assert_eq!(tally.mixed, 0);
        assert_eq!(tally.uniform, 1);
    }

    #[test]
    fn a_union_sharing_a_granule_with_a_member_of_another_type_does_disagree() {
        let mut types = Types::new();
        let mut names = Interner::new();
        let long = types.int(IntKind::Long);
        let double = types.float(crate::kind::FloatKind::Double);
        let members = [FieldDecl::new(Some(names.intern("i")), long), FieldDecl::new(None, double)];
        let inner = types.declare_record(RecordKind::Union, None);
        let laid_out = layout_record(
            &types,
            RecordKind::Union,
            &members,
            &RecordOptions::default(),
            &target(),
        )
        .expect("a union with a layout");
        types.complete_record(inner, laid_out);
        let inner = types.record(inner);
        let int = types.int(IntKind::Int);
        let id = built(&mut types, &mut names, &[("u", inner), ("n", int)]);

        // Whichever member the union holds, the `int` after it is a second type in the same
        // sixteen bytes, so both layouts disagree and so does the granule.
        let tally = measure(&types, id, &target(), Keying::Exact, 16).expect("a complete record");
        assert_eq!(tally.bytes, 16);
        assert_eq!(tally.mixed, 1);
    }

    #[test]
    fn two_pointers_to_different_things_agree_only_under_the_looser_keying() {
        let mut types = Types::new();
        let mut names = Interner::new();
        let ch = types.int(IntKind::Char);
        let int = types.int(IntKind::Int);
        let to_char = types.pointer(ch);
        let to_int = types.pointer(int);
        let id = built(&mut types, &mut names, &[("a", to_char), ("b", to_int)]);

        let exact = measure(&types, id, &target(), Keying::Exact, 16).expect("a complete record");
        assert_eq!(exact.mixed, 1);
        let loose = measure(&types, id, &target(), Keying::PointersTogether, 16)
            .expect("a complete record");
        assert_eq!(loose.mixed, 0);
        assert_eq!(loose.uniform, 1);
    }

    #[test]
    fn an_enumeration_agrees_with_the_integer_it_is_represented_in() {
        let mut types = Types::new();
        let mut names = Interner::new();
        let int = types.int(IntKind::Int);
        let enumeration = types.declare_enum(None);
        types.complete_enum(enumeration, int, false);
        let enumeration = types.enumeration(enumeration);
        let id = built(&mut types, &mut names, &[("a", int), ("b", enumeration)]);

        let tally = measure(&types, id, &target(), Keying::Exact, 16).expect("a complete record");
        assert_eq!(tally.mixed, 0);
        assert_eq!(tally.uniform, 1);
    }

    #[test]
    fn a_flexible_array_member_paints_nothing_because_it_occupies_nothing() {
        let mut types = Types::new();
        let mut names = Interner::new();
        let long = types.int(IntKind::Long);
        let ch = types.int(IntKind::Char);
        let flexible = types.array(ch, ArrayLen::Unknown);
        let id = built(&mut types, &mut names, &[("a", long), ("rest", flexible)]);

        let tally = measure(&types, id, &target(), Keying::Exact, 16).expect("a complete record");
        assert_eq!(tally.bytes, 8);
        assert_eq!(tally.uniform, 1);
        assert_eq!(tally.mixed, 0);
    }

    #[test]
    fn the_ratio_is_a_quarter_when_nothing_disagrees_and_four_when_everything_does() {
        let none = Tally { uniform: 4, ..Tally::default() };
        assert!((none.ratio(16) - 0.25).abs() < 1e-9);
        let all = Tally { mixed: 4, ..Tally::default() };
        assert!((all.ratio(16) - 4.25).abs() < 1e-9);
        // At sixteen the budget of 1.25 bytes per byte is spent exactly by a quarter of the
        // granules disagreeing. At eight the entry costs twice as much per byte, so the same
        // budget only pays for three sixteenths of them.
        let budget = Tally { uniform: 3, mixed: 1, ..Tally::default() };
        assert!((budget.ratio(16) - 1.25).abs() < 1e-9);
        let budget = Tally { uniform: 13, mixed: 3, ..Tally::default() };
        assert!((budget.ratio(8) - 1.25).abs() < 1e-9);
    }

    #[test]
    fn the_default_granule_is_eight_because_a_pointer_and_two_ints_fit_in_sixteen() {
        // The whole finding in one record. `struct { char *p; int a; int b; }` is an ordinary
        // shape and it disagrees at sixteen bytes and agrees at eight, which is why the
        // measurement moved the granule and why the default is what it is.
        let mut types = Types::new();
        let mut names = Interner::new();
        let ch = types.int(IntKind::Char);
        let int = types.int(IntKind::Int);
        let to_char = types.pointer(ch);
        let id = built(&mut types, &mut names, &[("p", to_char), ("a", int), ("b", int)]);

        let wide = measure(&types, id, &target(), Keying::Exact, 16).expect("a complete record");
        assert_eq!(wide.mixed, 1);
        assert_eq!(wide.uniform, 0);

        assert_eq!(GRANULE, 8);
        let tally =
            measure(&types, id, &target(), Keying::Exact, GRANULE).expect("a complete record");
        assert_eq!(tally.mixed, 0);
        assert_eq!(tally.uniform, 2);
    }
}
