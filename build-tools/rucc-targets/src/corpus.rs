//! Generating the record layout corpus that a reference compiler is asked to agree with.
//!
//! Design: `spec/cross-compile/14-testing.md` section 14.3 and `spec/cross-compile/06-abis.md`
//! section 6.9. This is the static half of milestone M6.5.
//!
//! What it writes is C that contains no code: a few hundred record declarations and a
//! `_Static_assert` for every size, every alignment and every ordinary member's offset. The
//! numbers in the assertions are what rucc thinks, so a file that does not compile under gcc or
//! clang for the same target is a disagreement about layout, printed as a diagnostic with the
//! declaration attached. It compiles with `-c` and no libc, which is what makes it usable for the
//! freestanding rows as well as the hosted ones.
//!
//! The numbers come from [`rucc_types::layout_record`], the same function the compiler calls when
//! it parses a `struct`, rather than from a description of it written for this file. A corpus that
//! checks a second implementation against a reference tells you nothing about the first one, and
//! `spec/cross-compile/04-target-matrix.md` section 4.7 is the rule that says so.
//!
//! The shapes are generated from a seed and the seed is a constant, so the corpus is the same on
//! every machine and, more usefully, the same for every target. Two targets' files differ only in
//! the numbers, so `diff` between them is a list of the layout decisions the two targets make
//! differently and nothing else.
//!
//! # What it does not cover, and why
//!
//! Every row of the target table has a file. It did not always: a record layout needed a three
//! field triple, which could spell fifteen of the forty two, and the other twenty seven were
//! skipped and counted. [`TargetInfo::for_tuple`] closed that.
//!
//! `__int128` is the one type the shapes are not the same everywhere about. It does not exist on
//! i686, on 32-bit ARM or on RISC-V 32, so the two shapes that name it are written only for the
//! rows that have it, and it is kept out of the generated half so that every target's `gen_NN`
//! declarations stay identical and a diff between two files stays a list of layout decisions.
//!
//! Bit-field positions are not asserted directly. `offsetof` refuses a bit-field, so where a
//! bit-field starts is a question for a program that runs, which is the differential harness in
//! tamnd/rucc-cross rather than this. What is asserted here is the size and the alignment of a
//! record that contains bit-fields and the offset of the first ordinary member after them, and
//! that is enough to catch a width allocated in the wrong unit, a zero width member that did not
//! push, and a straddle that went the wrong way.

use std::fmt::Write as _;
use std::path::Path;
use std::process::ExitCode;

use rucc_base::Interner;
use rucc_target::TargetInfo;
use rucc_tuple::{TARGETS, TargetTuple};
use rucc_types::{
    ArrayLen, FieldDecl, FloatKind, IntKind, RecordKind, RecordOptions, TypeId, Types, declare,
    layout_record,
};

use crate::rng::Rng;

/// Where the generated files live, relative to the workspace root.
const DIR: &str = "tests/abi-corpus";

/// The seed the generated half of the corpus is drawn from.
///
/// A constant rather than an argument, because the corpus is checked in and a corpus that changes
/// when somebody runs the command with a different number is a corpus that produces a diff nobody
/// asked for. Changing this is a deliberate act that regenerates every file.
const SEED: u64 = 0x5243_4300_4d36_2e35;

/// How many records the generated half declares.
///
/// Enough that the member orders cover the cases nobody thinks to write by hand, small enough that
/// the file stays something a person can read when one assertion in it fails.
const GENERATED: usize = 48;

/// What to do with the corpus.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Write it to `tests/abi-corpus`.
    Write,
    /// Check that what is there matches what would be written.
    Check,
}

/// Print the corpus for one target on standard output.
pub(crate) fn one(target: TargetTuple) -> ExitCode {
    print!("{}", render(target, &TargetInfo::for_tuple(target)));
    ExitCode::SUCCESS
}

/// Write or check every file.
pub(crate) fn run(root: &Path, mode: Mode) -> ExitCode {
    let dir = root.join(DIR);
    let mut written = 0;
    let mut stale = Vec::new();

    for entry in TARGETS {
        let Ok(target) = entry.tuple.parse::<TargetTuple>() else {
            eprintln!("error: the target table holds `{}`, which does not parse", entry.tuple);
            return ExitCode::FAILURE;
        };
        let path = dir.join(format!("{}.c", target.to_canonical_string()));
        let wanted = render(target, &TargetInfo::for_tuple(target));

        if mode == Mode::Check {
            if std::fs::read_to_string(&path).unwrap_or_default() != wanted {
                stale.push(entry.tuple);
            }
            written += 1;
            continue;
        }

        if let Err(error) = std::fs::create_dir_all(&dir) {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
        if let Err(error) = std::fs::write(&path, wanted) {
            eprintln!("error: {}: {error}", path.display());
            return ExitCode::FAILURE;
        }
        written += 1;
    }

    if mode == Mode::Check {
        if stale.is_empty() {
            println!("abi-corpus: {written} files are up to date");
            return ExitCode::SUCCESS;
        }
        println!("abi-corpus: {} files are out of date, run `cargo xtask abi-corpus`", stale.len());
        for tuple in stale {
            println!("  {tuple}");
        }
        return ExitCode::FAILURE;
    }
    println!("abi-corpus: wrote {written} files to {DIR}");
    ExitCode::SUCCESS
}

/// A leaf type in the grammar, meaning one that does not name another record.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Leaf {
    Int(IntKind),
    Float(FloatKind),
    Pointer,
}

/// The types a member may be drawn from.
///
/// Every one of them exists on every row of the target table, which is what lets the generated
/// half of the corpus be the same source everywhere. `__int128` is not here for exactly that
/// reason: i686, 32-bit ARM and RISC-V 32 do not have it, and a leaf that appears on some rows and
/// not on others would make every `gen_NN` declaration differ between the two groups. It is
/// covered by two hand written shapes instead, which are simply absent on the rows without it.
const LEAVES: &[Leaf] = &[
    Leaf::Int(IntKind::Char),
    Leaf::Int(IntKind::SChar),
    Leaf::Int(IntKind::UChar),
    Leaf::Int(IntKind::Short),
    Leaf::Int(IntKind::UShort),
    Leaf::Int(IntKind::Int),
    Leaf::Int(IntKind::UInt),
    Leaf::Int(IntKind::Long),
    Leaf::Int(IntKind::ULong),
    Leaf::Int(IntKind::LongLong),
    Leaf::Float(FloatKind::Float),
    Leaf::Float(FloatKind::Double),
    Leaf::Float(FloatKind::LongDouble),
    Leaf::Pointer,
];

/// The types a bit-field may be declared in.
///
/// `_Bool` is not here and neither is `__int128`. A `_Bool` bit-field has a width of one and
/// nothing to vary, and an `__int128` one is an extension the two references do not agree about,
/// which would make a disagreement in this corpus a fact about the reference rather than about the
/// target.
const BIT_BASES: &[IntKind] =
    &[IntKind::Char, IntKind::UChar, IntKind::Short, IntKind::UShort, IntKind::Int, IntKind::UInt];

/// What a member is, before a target has been chosen.
#[derive(Clone, Copy)]
struct Member {
    /// The member's type, either a leaf or an earlier record by index.
    ty: MemberType,
    /// How many elements, when the member is an array. One means it is not one.
    elements: u64,
    /// The bit-field width, absent when the member is an ordinary one.
    bits: Option<u32>,
    /// An alignment the member asked for with `_Alignas`.
    align: Option<u64>,
}

#[derive(Clone, Copy)]
enum MemberType {
    Leaf(Leaf),
    Record(usize),
}

/// A record, before a target has been chosen.
struct Shape {
    /// The name, which is also the tag.
    name: String,
    /// Why it is in the corpus, printed above it.
    why: &'static str,
    kind: RecordKind,
    options: RecordOptions,
    members: Vec<Member>,
    /// Whether the last member is an array with no size.
    flexible: bool,
}

impl Member {
    fn leaf(leaf: Leaf) -> Member {
        Member { ty: MemberType::Leaf(leaf), elements: 1, bits: None, align: None }
    }

    fn bit_field(base: IntKind, bits: u32) -> Member {
        Member { ty: MemberType::Leaf(Leaf::Int(base)), elements: 1, bits: Some(bits), align: None }
    }
}

/// The whole corpus as shapes, in the order they are declared.
///
/// Two halves. The first is written by hand and every entry in it is a case somebody named: the
/// checklist in the M6.5 issue asks for bit-fields of every width, the zero width member, the one
/// that straddles a storage unit, `_Alignas`, `long double`, `__int128` and the empty struct. The
/// second is drawn from the seed, and it is there for the orders nobody thinks to write down.
///
/// `has_int128` is the one thing about the target this needs. The two shapes that name the type
/// are written only where it exists, and they are written past the point where a shape may be
/// nested so that leaving them out cannot move an index the generated half depends on.
fn shapes(has_int128: bool) -> Vec<Shape> {
    let mut shapes = Vec::new();

    // The two records every later shape is allowed to nest, declared first so that an index into
    // this list is always an index at something already complete.
    shapes.push(Shape {
        name: "nest_small".to_string(),
        why: "A small struct with an alignment of its own, nested by the shapes below.",
        kind: RecordKind::Struct,
        options: RecordOptions::default(),
        members: vec![
            Member::leaf(Leaf::Int(IntKind::Int)),
            Member::leaf(Leaf::Int(IntKind::Char)),
        ],
        flexible: false,
    });
    shapes.push(Shape {
        name: "nest_union".to_string(),
        why: "A union nested by the shapes below, because a union member's alignment is the \
              largest of its members and not the first one's.",
        kind: RecordKind::Union,
        options: RecordOptions::default(),
        members: vec![
            Member::leaf(Leaf::Int(IntKind::Char)),
            Member::leaf(Leaf::Float(FloatKind::Double)),
        ],
        flexible: false,
    });

    shapes.push(Shape {
        name: "empty".to_string(),
        why: "The empty struct, which C does not have and both references do. It is a GNU \
              extension with a size of zero, and C++ gives it a size of one, so this is the one \
              case where being right means disagreeing with the other language.",
        kind: RecordKind::Struct,
        options: RecordOptions::default(),
        members: Vec::new(),
        flexible: false,
    });

    // Every width in every base type. The interesting numbers are the ones at the ends: a width
    // equal to the type is a whole storage unit, and a width one less is the one that makes the
    // next member straddle.
    for base in BIT_BASES {
        let capacity = bit_capacity(*base);
        let mut members = Vec::new();
        for width in 1..=capacity {
            members.push(Member::bit_field(*base, width));
        }
        shapes.push(Shape {
            name: format!("bits_ladder_{}", base_name(*base)),
            why: "One bit-field of every width the type has, in order. The size of the whole is \
                  the sum rounded up, and a target that allocates into the wrong unit gets a \
                  different answer at the first width that does not fit.",
            kind: RecordKind::Struct,
            options: RecordOptions::default(),
            members,
            flexible: false,
        });
    }

    // The straddle. A field that does not fit in what is left of the current unit either moves to
    // the next one or is split across the boundary, and the two answers give different sizes.
    for base in BIT_BASES {
        let capacity = bit_capacity(*base);
        shapes.push(Shape {
            name: format!("bits_straddle_{}", base_name(*base)),
            why: "A width that fills all but one bit of a unit, followed by one that cannot fit \
                  in the bit that is left. Whether the second one starts a new unit or is split \
                  across the boundary is the difference this measures.",
            kind: RecordKind::Struct,
            options: RecordOptions::default(),
            members: vec![
                Member::bit_field(*base, capacity - 1),
                Member::bit_field(*base, capacity),
                Member::leaf(Leaf::Int(IntKind::Char)),
            ],
            flexible: false,
        });
    }

    // The zero width member, which has to be unnamed and whose only job is to push.
    shapes.push(Shape {
        name: "bits_zero_width".to_string(),
        why: "The zero width bit-field. It holds nothing and occupies no bits, and the member \
              after it starts at the next boundary of its type, so the offset of the char at the \
              end is the whole of what this asserts.",
        kind: RecordKind::Struct,
        options: RecordOptions::default(),
        members: vec![
            Member::bit_field(IntKind::UInt, 3),
            Member::bit_field(IntKind::UInt, 0),
            Member::bit_field(IntKind::UInt, 5),
            Member::leaf(Leaf::Int(IntKind::Char)),
        ],
        flexible: false,
    });
    shapes.push(Shape {
        name: "bits_zero_width_only".to_string(),
        why: "A struct whose only member is a zero width bit-field. It has a size of zero on both \
              references and an alignment that is the base type's, which is the one place a zero \
              width member changes something other than an offset.",
        kind: RecordKind::Struct,
        options: RecordOptions::default(),
        members: vec![Member::bit_field(IntKind::UInt, 0)],
        flexible: false,
    });

    // A bit-field next to an ordinary member, in both orders, because the boundary between the two
    // is where a compiler that keeps a running bit position gets it wrong.
    shapes.push(Shape {
        name: "bits_then_member".to_string(),
        why: "A bit-field that does not fill its unit followed by an ordinary member. The \
              ordinary one starts at its own alignment and the bits before it are padding.",
        kind: RecordKind::Struct,
        options: RecordOptions::default(),
        members: vec![
            Member::bit_field(IntKind::UInt, 3),
            Member::leaf(Leaf::Int(IntKind::Int)),
            Member::bit_field(IntKind::UInt, 3),
            Member::leaf(Leaf::Int(IntKind::Char)),
        ],
        flexible: false,
    });

    // `_Alignas` on a member. C 6.7.5 makes a number below the member's own alignment a
    // constraint violation rather than a request that is ignored, and both references refuse it,
    // so the ladder is on a `char` where every power of two is a raise. The two shapes after it
    // put a large alignment on a member that already has one, which is the other half of the rule
    // and the half that moves the record's own alignment.
    for align in [1u64, 2, 4, 8, 16, 32] {
        shapes.push(Shape {
            name: format!("alignas_{align}"),
            why: "`_Alignas` on a char member. It raises the member's alignment and the record's \
                  with it, so the one byte case is a char that stays where it was and the thirty \
                  two byte case moves everything after it.",
            kind: RecordKind::Struct,
            options: RecordOptions::default(),
            members: vec![
                Member::leaf(Leaf::Int(IntKind::Char)),
                Member {
                    ty: MemberType::Leaf(Leaf::Int(IntKind::Char)),
                    elements: 1,
                    bits: None,
                    align: Some(align),
                },
                Member::leaf(Leaf::Int(IntKind::Char)),
            ],
            flexible: false,
        });
    }
    for (name, leaf) in [
        ("alignas_over_int", Leaf::Int(IntKind::Int)),
        ("alignas_over_long_double", Leaf::Float(FloatKind::LongDouble)),
    ] {
        shapes.push(Shape {
            name: name.to_string(),
            why: "`_Alignas(32)` on a member that already has an alignment of its own. Thirty two \
                  is above every scalar alignment on every target, so this is the same request \
                  everywhere and the answer still differs where the member's size does.",
            kind: RecordKind::Struct,
            options: RecordOptions::default(),
            members: vec![
                Member::leaf(Leaf::Int(IntKind::Char)),
                Member { ty: MemberType::Leaf(leaf), elements: 1, bits: None, align: Some(32) },
                Member::leaf(Leaf::Int(IntKind::Char)),
            ],
            flexible: false,
        });
    }

    // The two types the targets disagree about most. `long double` is eight bytes on Apple's
    // AArch64 and under MSVC, ten in sixteen on SysV x86-64 and mingw, and true quad on AArch64
    // Linux and RISC-V, and every one of those gives this struct a different size.
    shapes.push(Shape {
        name: "long_double_pair".to_string(),
        why: "A char in front of a `long double`, which is the shortest program that tells the \
              four `long double` answers apart.",
        kind: RecordKind::Struct,
        options: RecordOptions::default(),
        members: vec![
            Member::leaf(Leaf::Int(IntKind::Char)),
            Member::leaf(Leaf::Float(FloatKind::LongDouble)),
        ],
        flexible: false,
    });
    // The five shapes that tell the two bit-field rules apart. Everything above this either uses
    // one declared type throughout or has no bit-fields in it, and the Itanium rule and the
    // Microsoft one agree on all of that, which is how the first version of this corpus managed to
    // be wrong about Windows in only two places.
    shapes.push(Shape {
        name: "bits_mixed_bases".to_string(),
        why: "Bit-fields of four different declared types in a row, none of them full. The \
              Itanium rule packs them into whatever storage they reach and Microsoft's opens a \
              new unit every time the size changes, so this is four bytes under one and twelve \
              under the other.",
        kind: RecordKind::Struct,
        options: RecordOptions::default(),
        members: vec![
            Member::bit_field(IntKind::UInt, 3),
            Member::bit_field(IntKind::UShort, 5),
            Member::bit_field(IntKind::UChar, 3),
            Member::bit_field(IntKind::UInt, 3),
        ],
        flexible: false,
    });
    shapes.push(Shape {
        name: "bits_after_member".to_string(),
        why: "An ordinary member in front of a bit-field wider than the space left over. The \
              Itanium rule puts it at the next free bit because it still fits inside one unit of \
              its own type, and Microsoft's opens a unit at the type's alignment, so the two \
              differ by a whole eight bytes here.",
        kind: RecordKind::Struct,
        options: RecordOptions::default(),
        members: vec![
            Member::leaf(Leaf::Int(IntKind::Char)),
            Member {
                ty: MemberType::Leaf(Leaf::Int(IntKind::LongLong)),
                elements: 1,
                bits: Some(33),
                align: None,
            },
        ],
        flexible: false,
    });
    shapes.push(Shape {
        name: "bits_trailing_zero_width".to_string(),
        why: "A zero width bit-field as the last member. The padding it opens belongs to the \
              record even with nothing after it to occupy it, except under Microsoft's rule where \
              a zero width member with no run of bit-fields in front of it ends nothing and so \
              does nothing.",
        kind: RecordKind::Struct,
        options: RecordOptions::default(),
        members: vec![Member::leaf(Leaf::Int(IntKind::Char)), Member::bit_field(IntKind::UInt, 0)],
        flexible: false,
    });
    shapes.push(Shape {
        name: "union_of_bits".to_string(),
        why: "A union of a bit-field and a char. Microsoft's rule gives the bit-field its \
              storage and no say in the alignment, so this is four bytes aligned to one there, \
              which is an alignment smaller than either member has on its own.",
        kind: RecordKind::Union,
        options: RecordOptions::default(),
        members: vec![Member::bit_field(IntKind::UInt, 3), Member::leaf(Leaf::Int(IntKind::Char))],
        flexible: false,
    });
    // Nothing below this line may be nested inside anything, so the count of what may is taken
    // here. A struct with a flexible array member is not a member type: putting one anywhere but
    // last is a GNU extension the reference refuses under `-Werror`, and a shape drawn from a seed
    // has no way to promise it drew that one last.
    let nestable = shapes.len();

    // The two shapes that name `__int128`, on the rows that have the type. They are here rather
    // than up with the other scalars because a shape declared above `nestable` is one the seeded
    // half may nest by index, and an index that means a different record on i686 than it does on
    // x86-64 would make the generated half of the corpus a different corpus per architecture.
    if has_int128 {
        shapes.push(Shape {
            name: "int128_pair".to_string(),
            why: "A char in front of an `__int128`. It is sixteen bytes aligned to sixteen \
                  everywhere except s390x, which caps every scalar alignment at eight and so puts \
                  it at offset eight instead of sixteen.",
            kind: RecordKind::Struct,
            options: RecordOptions::default(),
            members: vec![
                Member::leaf(Leaf::Int(IntKind::Char)),
                Member::leaf(Leaf::Int(IntKind::Int128)),
            ],
            flexible: false,
        });
        shapes.push(Shape {
            name: "union_of_the_widest".to_string(),
            why: "A union of the three widest scalars. Its size is the largest member rounded up \
                  to the alignment, and both of those move between targets.",
            kind: RecordKind::Union,
            options: RecordOptions::default(),
            members: vec![
                Member::leaf(Leaf::Int(IntKind::Int128)),
                Member::leaf(Leaf::Float(FloatKind::LongDouble)),
                Member::leaf(Leaf::Pointer),
            ],
            flexible: false,
        });
    }

    // The flexible array member, which is laid out where it would have been and contributes
    // nothing, because `malloc(sizeof(struct S) + n)` depends on exactly that.
    shapes.push(Shape {
        name: "flexible".to_string(),
        why: "A flexible array member. It sits where it would have sat and adds nothing to the \
              size, and the tail padding in front of it is what makes the idiom allocate enough.",
        kind: RecordKind::Struct,
        options: RecordOptions::default(),
        members: vec![
            Member::leaf(Leaf::Int(IntKind::Int)),
            Member::leaf(Leaf::Int(IntKind::Char)),
            Member {
                ty: MemberType::Leaf(Leaf::Int(IntKind::Int)),
                elements: 0,
                bits: None,
                align: None,
            },
        ],
        flexible: true,
    });
    shapes.push(Shape {
        name: "flexible_only".to_string(),
        why: "A struct whose only member is a flexible array member, so it holds no storage at \
              all. It has the size of the empty struct and the alignment of the element type, \
              which is the one place those two come from different members.",
        kind: RecordKind::Struct,
        options: RecordOptions::default(),
        members: vec![Member {
            ty: MemberType::Leaf(Leaf::Int(IntKind::Int)),
            elements: 0,
            bits: None,
            align: None,
        }],
        flexible: true,
    });

    let mut rng = Rng::new(SEED);
    for index in 0..GENERATED {
        let kind = if rng.below(4) == 0 { RecordKind::Union } else { RecordKind::Struct };
        let count = 1 + rng.below(6) as usize;
        let mut members = Vec::with_capacity(count);
        for _ in 0..count {
            members.push(random_member(&mut rng, nestable));
        }
        // A union of bit-fields is legal and the reference disagrees with nothing about it, so it
        // is not worth a row. A union whose members are ordinary types is.
        if kind == RecordKind::Union {
            for member in &mut members {
                member.bits = None;
            }
        }
        shapes.push(Shape {
            name: format!("gen_{index:02}"),
            why: "",
            kind,
            options: RecordOptions::default(),
            members,
            flexible: false,
        });
    }
    shapes
}

/// One member, drawn from the seed.
fn random_member(rng: &mut Rng, nestable: usize) -> Member {
    // Nesting one record in seven. More than that and the corpus becomes a test of how deeply a
    // compiler recurses rather than of where it puts things.
    let ty = if rng.below(7) == 0 {
        MemberType::Record(rng.below(nestable as u64) as usize)
    } else {
        MemberType::Leaf(LEAVES[rng.below(LEAVES.len() as u64) as usize])
    };

    // Arrays one time in five, and never longer than three, because an array's contribution is its
    // element size times its length and the length is the least interesting of the two.
    let elements = if rng.below(5) == 0 { 2 + rng.below(2) } else { 1 };

    let mut member = Member { ty, elements, bits: None, align: None };

    // A bit-field one time in four, and only where the member is an integer of a type a bit-field
    // may be declared in.
    if elements == 1 && rng.below(4) == 0 {
        if let MemberType::Leaf(Leaf::Int(base)) = ty {
            if BIT_BASES.contains(&base) {
                member.bits = Some(1 + rng.below(u64::from(bit_capacity(base))) as u32);
                return member;
            }
        }
    }

    // `_Alignas` one time in nine. Sixteen and thirty two only, because a number below the
    // member's own alignment is a constraint violation rather than a request that is ignored, and
    // the member's own alignment is a fact about the target while the shapes are not. Sixteen is
    // above every scalar alignment on every row of the table, so both numbers are a raise
    // everywhere and the corpus stays the same source for every target.
    if rng.below(9) == 0 {
        member.align = Some(if rng.below(2) == 0 { 16 } else { 32 });
    }
    member
}

/// How many bits a bit-field of this type may have.
///
/// Written from the type's own size in the C sense rather than from the target, because these six
/// types are the same width on every row of the table: `char` is eight bits, `short` is sixteen
/// and `int` is thirty two everywhere, including on the sixteen bit data models this table does
/// not have. It is the ladder's upper bound and nothing else reads it, so a row where `int` is not
/// thirty two bits would be the thing that makes this take a target.
fn bit_capacity(base: IntKind) -> u32 {
    match base {
        IntKind::Char | IntKind::SChar | IntKind::UChar => 8,
        IntKind::Short | IntKind::UShort => 16,
        _ => 32,
    }
}

/// The part of a name that says which type a shape's bit-fields are.
fn base_name(base: IntKind) -> &'static str {
    match base {
        IntKind::Char => "char",
        IntKind::SChar => "schar",
        IntKind::UChar => "uchar",
        IntKind::Short => "short",
        IntKind::UShort => "ushort",
        IntKind::UInt => "uint",
        _ => "int",
    }
}

/// The corpus for one target, as the text of a C file.
fn render(target: TargetTuple, info: &TargetInfo) -> String {
    let shapes = shapes(info.scalars.has_int128);
    let mut types = Types::new();
    let mut names = Interner::new();
    let mut built: Vec<TypeId> = Vec::with_capacity(shapes.len());

    let mut out = String::new();
    header(&mut out, target, info.scalars.has_int128);

    for shape in &shapes {
        let tag = names.intern(&shape.name);
        let record = types.declare_record(shape.kind, Some(tag));
        let id = types.record(record);

        let mut decls = Vec::with_capacity(shape.members.len());
        for (index, member) in shape.members.iter().enumerate() {
            let last = index + 1 == shape.members.len();
            let base = match member.ty {
                MemberType::Leaf(Leaf::Int(kind)) => types.int(kind),
                MemberType::Leaf(Leaf::Float(kind)) => types.float(kind),
                MemberType::Leaf(Leaf::Pointer) => {
                    let void = types.void();
                    types.pointer(void)
                }
                MemberType::Record(at) => built[at],
            };
            let ty = if shape.flexible && last {
                types.array(base, ArrayLen::Unknown)
            } else if member.elements == 1 {
                base
            } else {
                types.array(base, ArrayLen::Fixed(member.elements))
            };
            // A zero width bit-field has to be unnamed, and everything else is named after its
            // position so that a failing assertion says which member it is about.
            let name = match member.bits {
                Some(0) => None,
                _ => Some(names.intern(&format!("m{index}"))),
            };
            decls.push(FieldDecl {
                name,
                ty,
                bits: member.bits,
                align: member.align,
                packed: false,
            });
        }

        let laid_out = match layout_record(&types, shape.kind, &decls, &shape.options, info) {
            Ok(laid_out) => laid_out,
            Err(error) => {
                // A shape the engine refuses is a bug in the grammar above rather than a fact
                // about the target, so it is worth saying loudly rather than skipping.
                panic!(
                    "{}: {} does not lay out: {error}",
                    target.to_canonical_string(),
                    shape.name
                );
            }
        };

        declaration(&mut out, &types, &names, shape, &decls, laid_out.fields.as_slice());
        assertions(&mut out, &names, shape, &decls, &laid_out);

        types.complete_record(record, laid_out);
        built.push(id);
    }

    out
}

/// The lines the header carries about `__int128`, which is nothing at all where the type exists.
///
/// The absence is worth saying out loud rather than leaving to be noticed, because two shapes are
/// missing from this file and somebody diffing it against x86-64's would otherwise have to work
/// out whether they were dropped on purpose or lost.
fn int128_note(has_int128: bool) -> impl Iterator<Item = &'static str> {
    let note: &'static [&'static str] = if has_int128 {
        &[]
    } else {
        &[
            "",
            "This target has no `__int128`, so the two shapes that name it are not in this file.",
            "That is the only thing the files disagree about declaring rather than about numbers.",
        ]
    };
    note.iter().copied()
}

/// The comment at the top of a generated file.
fn header(out: &mut String, target: TargetTuple, has_int128: bool) {
    let canonical = target.to_canonical_string();
    let _ = writeln!(out, "/* Record layout for {canonical}, as rucc computes it.");
    for line in [
        "",
        "Generated by `cargo run -q -p rucc-targets -- abi-corpus --write`. Do not edit this",
        "file, edit the grammar in `build-tools/rucc-targets/src/corpus.rs`.",
        "",
        "Every number below comes from `rucc_types::layout_record`, which is the function the",
        "compiler calls when it parses a struct. So this file failing to compile under gcc or",
        "clang for this target is a disagreement about layout between rucc and the reference,",
        "and the diagnostic names the record and the member it is about.",
        "",
        "It compiles with `-c -std=c17 -Wall -Wextra -Werror` and no libc, which is what lets",
        "the freestanding rows be checked the same way the hosted ones are.",
        "",
        "The shapes are the same in every target's file and only the numbers differ, so a diff",
        "between two of these is the list of layout decisions the two targets make differently.",
    ]
    .into_iter()
    .chain(int128_note(has_int128))
    {
        // No trailing space on the blank lines, because the house rule against trailing
        // whitespace applies to what a generator writes as much as to what a person types.
        if line.is_empty() {
            out.push_str(" *\n");
        } else {
            let _ = writeln!(out, " * {line}");
        }
    }
    out.push_str(" */\n\n");
}

/// The declaration of one record, with the reason for it above.
fn declaration(
    out: &mut String,
    types: &Types,
    names: &Interner,
    shape: &Shape,
    decls: &[FieldDecl],
    fields: &[rucc_types::Field],
) {
    if !shape.why.is_empty() {
        out.push_str("/* ");
        // The reason wrapped at eighty columns, continued with the comment's own leading space so
        // that the text lines up under itself.
        let mut column = 3;
        for word in shape.why.split_whitespace() {
            if column + word.len() > 96 {
                out.push_str("\n * ");
                column = 3;
            } else if column > 3 {
                out.push(' ');
                column += 1;
            }
            out.push_str(word);
            column += word.len();
        }
        out.push_str(" */\n");
    }

    let keyword = match shape.kind {
        RecordKind::Struct => "struct",
        RecordKind::Union => "union",
    };
    let _ = writeln!(out, "{keyword} {} {{", shape.name);
    for (decl, field) in decls.iter().zip(fields) {
        out.push('\t');
        if let Some(align) = decl.align {
            let _ = write!(out, "_Alignas({align}) ");
        }
        match (decl.name, decl.bits) {
            // An unnamed member is only ever a zero width bit-field here, and the type it is
            // declared in is what decides where the next member starts.
            (None, Some(bits)) => {
                let _ = write!(out, "{} : {bits}", rucc_types::spell(types, names, decl.ty));
            }
            (Some(name), Some(bits)) => {
                let _ = write!(out, "{} : {bits}", declare(types, names, decl.ty, name));
            }
            (Some(name), None) => out.push_str(&declare(types, names, decl.ty, name)),
            (None, None) => unreachable!("only a zero width bit-field is unnamed here"),
        }
        let _ = writeln!(out, ";{}", offset_note(field));
    }
    out.push_str("};\n");
}

/// Where a member ended up, as a trailing comment, so a reader does not have to find the assertion.
fn offset_note(field: &rucc_types::Field) -> String {
    match field.bits {
        Some(_) => format!("\t/* bit {} */", field.bit_offset()),
        None => format!("\t/* +{} */", field.offset),
    }
}

/// The assertions for one record.
fn assertions(
    out: &mut String,
    names: &Interner,
    shape: &Shape,
    decls: &[FieldDecl],
    laid_out: &rucc_types::RecordLayout,
) {
    let keyword = match shape.kind {
        RecordKind::Struct => "struct",
        RecordKind::Union => "union",
    };
    let name = &shape.name;
    let _ = writeln!(
        out,
        "_Static_assert(sizeof({keyword} {name}) == {}, \"sizeof {keyword} {name}\");",
        laid_out.layout.size
    );
    let _ = writeln!(
        out,
        "_Static_assert(_Alignof({keyword} {name}) == {}, \"_Alignof {keyword} {name}\");",
        laid_out.layout.align
    );

    // `offsetof` refuses a bit-field, so the ordinary members are the ones with an offset to
    // assert. That is not a hole: a bit-field allocated in the wrong place moves the next ordinary
    // member, and every shape with bit-fields in it has one.
    for (decl, field) in decls.iter().zip(&laid_out.fields) {
        if field.bits.is_some() {
            continue;
        }
        let Some(symbol) = decl.name else { continue };
        let member = names.resolve(symbol);
        let _ = writeln!(
            out,
            "_Static_assert(__builtin_offsetof({keyword} {name}, {member}) == {}, \"offsetof {keyword} {name}.{member}\");",
            field.offset
        );
    }
    out.push('\n');
}
