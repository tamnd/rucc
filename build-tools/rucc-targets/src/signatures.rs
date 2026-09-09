//! The signature corpus the differential ABI harness runs.
//!
//! Design: `spec/cross-compile/14-testing.md` section 14.3, which is layer 6 of the ladder in
//! section 14.1, and `spec/cross-compile/06-abis.md` section 6.2 items 4 to 6.
//!
//! # What this is for
//!
//! `tests/abi-corpus` asks whether two compilers agree about where the members of a struct are.
//! This asks the question that has no static answer: whether they agree about which register a
//! struct arrives in, which ones it spills to the stack from, whether it travels as a copy or as
//! a pointer to one, and where the return value comes back. None of that is visible in the source
//! and none of it can be asserted with `_Static_assert`, so the only way to check it is to compile
//! one side of a call with one compiler and the other side with the other, run the program, and
//! see whether the values arrived.
//!
//! Section 14.3's argument for doing this at all is document 01.8's result: ABI Cafe found GCC,
//! Clang and rustc disagreeing on x86-64 Linux, which is the most exercised ABI in existence.
//! Implementing the psABI document is not evidence.
//!
//! # The shape of the corpus
//!
//! Four files, generated together and checked in, and the same C for every target.
//!
//! `abi.h` holds the aggregate types and the prototypes. `callee.c` defines every function, and
//! each definition checks each of its parameters against the value the caller promised and
//! returns the value the caller expects. `caller.c` holds `main`, which calls every function with
//! those values and checks the return. Neither file can see the other's code, so a value that
//! arrives wrong is an ABI disagreement rather than a missed optimization.
//!
//! `report.c` is the fourth and it is the only one that includes a header. rucc has no built-in
//! system include directories, so a file under test that said `#include <stdio.h>` would be
//! testing whichever headers the machine happens to have rather than a calling convention. The
//! two sides call `abi_fail` with two `const char *` and nothing else, which is the one signature
//! every ABI here agrees about, and `report.c` is always built by the reference compiler.
//!
//! The values are generated rather than written, which is section 14.3's "value checking
//! generated rather than written". Each scalar in the program gets its own number, so a failure
//! names the function, the parameter and the member, and no two of them are the same value by
//! accident.
//!
//! # What is not here yet
//!
//! Variadic signatures, which are the next piece of this milestone and which are where Darwin
//! arm64 and Windows diverge from everyone else. `__int128` is out too, for the same reason it is
//! out of the seeded half of the record corpus: three rows of the target table do not have the
//! type, and one source for every target is worth more than a corpus that needs a preprocessor
//! conditional to say which types exist.

use std::fmt::Write as _;
use std::path::Path;
use std::process::ExitCode;

use crate::rng::Rng;

/// Where the generated files live, relative to the workspace root.
const DIR: &str = "tests/abi-signatures";

/// The seed the drawn signatures come from.
///
/// A constant rather than an argument, for the reason the record corpus has one: the files are
/// checked in, and a corpus that changes when somebody passes a different number is a diff nobody
/// asked for.
const SEED: u64 = 0x5243_4300_4162_6944;

/// How many signatures are drawn from the seed.
///
/// Enough to reach the argument registers and run past them on every ABI here, small enough that
/// the two files stay readable when one line of one of them fails.
const DRAWN: usize = 48;

/// How many variadic signatures are drawn from the same seed.
///
/// Fewer than the fixed ones because each is longer: a variadic definition carries the walk over
/// the arguments as well as the checks, so the same number would double the file for a case the
/// hand written ten already name the edges of.
const DRAWN_VARIADIC: usize = 24;

/// How many bytes the anchor array has, which is what a pointer argument points into.
const ANCHOR: u64 = 64;

/// What the generator was asked to do.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Write the files.
    Write,
    /// Check that what is on disk is what would be written.
    Check,
}

/// A scalar type, which is a type the grammar can hand a value to directly.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scalar {
    Char,
    SChar,
    UChar,
    Short,
    UShort,
    Int,
    UInt,
    Long,
    ULong,
    LongLong,
    ULongLong,
    Float,
    Double,
    LongDouble,
    Pointer,
}

impl Scalar {
    /// The C spelling.
    fn spelling(self) -> &'static str {
        match self {
            Scalar::Char => "char",
            Scalar::SChar => "signed char",
            Scalar::UChar => "unsigned char",
            Scalar::Short => "short",
            Scalar::UShort => "unsigned short",
            Scalar::Int => "int",
            Scalar::UInt => "unsigned int",
            Scalar::Long => "long",
            Scalar::ULong => "unsigned long",
            Scalar::LongLong => "long long",
            Scalar::ULongLong => "unsigned long long",
            Scalar::Float => "float",
            Scalar::Double => "double",
            Scalar::LongDouble => "long double",
            Scalar::Pointer => "void *",
        }
    }

    /// What the default argument promotions turn this into on the way past a `...`.
    ///
    /// C17 6.5.2.2p6, and it is why a variadic argument is read as a type the caller never wrote.
    /// Everything narrower than `int` becomes an `int`, because every target in the table has a
    /// thirty two bit `int` and a sixteen bit `short`, so `int` holds every value of every one of
    /// them and the unsigned ones do not stay unsigned. `float` becomes `double`. There is no way
    /// to pass a `float` through a `...` and there is no point pretending otherwise: a corpus that
    /// read one back would be asserting the opposite of what the standard says.
    ///
    /// This is why the value a variadic parameter carries is written in the narrow type and read
    /// in the wide one. The literal the caller passes is promoted by the same rule, so the two
    /// sides are comparing the same number and the comparison itself is well typed.
    fn promoted(self) -> Scalar {
        match self {
            Scalar::Char | Scalar::SChar | Scalar::UChar | Scalar::Short | Scalar::UShort => {
                Scalar::Int
            }
            Scalar::Float => Scalar::Double,
            other => other,
        }
    }
}

/// The scalars a drawn parameter may have.
///
/// `long` is in here and its width is not the same on every row, which is the point: a corpus
/// that only used types of one width would agree with itself on LLP64 by not asking. The values
/// below stay inside thirty two bits for it for that reason.
const SCALARS: &[Scalar] = &[
    Scalar::Char,
    Scalar::SChar,
    Scalar::UChar,
    Scalar::Short,
    Scalar::UShort,
    Scalar::Int,
    Scalar::UInt,
    Scalar::Long,
    Scalar::ULong,
    Scalar::LongLong,
    Scalar::ULongLong,
    Scalar::Float,
    Scalar::Double,
    Scalar::LongDouble,
    Scalar::Pointer,
];

/// Whether an aggregate is a struct or a union.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Struct,
    Union,
}

/// A member of an aggregate.
enum Member {
    /// A scalar, with its member name.
    Scalar(&'static str, Scalar),
    /// An earlier aggregate, by index, with its member name.
    Nested(&'static str, usize),
}

/// An aggregate the grammar may pass, return or nest.
struct Aggregate {
    name: &'static str,
    kind: Kind,
    /// Why it is here, written above it in the header.
    why: &'static str,
    members: &'static [Member],
}

/// The aggregates, in declaration order, each nesting only the ones above it.
///
/// Every one of them is a case some psABI treats differently from the next: the classification
/// rules are all written over sizes, over whether every member is a float, and over how many
/// registers are left, so these are the sizes and the shapes at the edges of those rules. Two
/// floats is a homogeneous aggregate under AAPCS64 and an eightbyte of two packed singles under
/// SysV. Four is the AAPCS64 limit. Twenty four bytes is past every threshold on every ABI here.
const AGGREGATES: &[Aggregate] = &[
    Aggregate {
        name: "one_char",
        kind: Kind::Struct,
        why: "One byte in a struct, which is a register on every ABI here and a different \
              register from the one a bare char would use on none of them.",
        members: &[Member::Scalar("a", Scalar::Char)],
    },
    Aggregate {
        name: "three_char",
        kind: Kind::Struct,
        why: "Three bytes, so the size is not a power of two and the last byte of the register \
              it travels in is nobody's.",
        members: &[
            Member::Scalar("a", Scalar::Char),
            Member::Scalar("b", Scalar::Char),
            Member::Scalar("c", Scalar::Char),
        ],
    },
    Aggregate {
        name: "two_int",
        kind: Kind::Struct,
        why: "Eight bytes of integer, which is one register everywhere and the smallest \
              aggregate that fills one.",
        members: &[Member::Scalar("a", Scalar::Int), Member::Scalar("b", Scalar::Int)],
    },
    Aggregate {
        name: "int_float",
        kind: Kind::Struct,
        why: "An int and a float in one eightbyte. SysV classifies the eightbyte by what is in \
              it, so this is an integer register and two floats in the same space are not.",
        members: &[Member::Scalar("a", Scalar::Int), Member::Scalar("b", Scalar::Float)],
    },
    Aggregate {
        name: "two_float",
        kind: Kind::Struct,
        why: "Two floats, which is a homogeneous aggregate in two vector registers under AAPCS64 \
              and one SSE register holding both under SysV.",
        members: &[Member::Scalar("a", Scalar::Float), Member::Scalar("b", Scalar::Float)],
    },
    Aggregate {
        name: "four_float",
        kind: Kind::Struct,
        why: "Four floats, which is the largest homogeneous aggregate AAPCS64 will put in \
              registers and sixteen bytes of SSE under SysV.",
        members: &[
            Member::Scalar("a", Scalar::Float),
            Member::Scalar("b", Scalar::Float),
            Member::Scalar("c", Scalar::Float),
            Member::Scalar("d", Scalar::Float),
        ],
    },
    Aggregate {
        name: "two_double",
        kind: Kind::Struct,
        why: "Sixteen bytes of floating point, which is two registers under both rules and a \
              hidden pointer under Windows x64.",
        members: &[Member::Scalar("a", Scalar::Double), Member::Scalar("b", Scalar::Double)],
    },
    Aggregate {
        name: "long_double_one",
        kind: Kind::Struct,
        why: "A long double in a struct, which is the x87 eighty bit type on two rows, an IEEE \
              quad on several, a double double on ppc64le and a plain double on MSVC.",
        members: &[Member::Scalar("a", Scalar::LongDouble)],
    },
    Aggregate {
        name: "int_pointer",
        kind: Kind::Struct,
        why: "An int and a pointer, so the size and the padding both move with the pointer \
              width and the member after the padding is what says whether they moved together.",
        members: &[Member::Scalar("a", Scalar::Int), Member::Scalar("b", Scalar::Pointer)],
    },
    Aggregate {
        name: "six_int",
        kind: Kind::Struct,
        why: "Twenty four bytes, which is past the threshold on every ABI here, so it travels \
              as a copy the callee is given the address of rather than in registers.",
        members: &[
            Member::Scalar("a", Scalar::Int),
            Member::Scalar("b", Scalar::Int),
            Member::Scalar("c", Scalar::Int),
            Member::Scalar("d", Scalar::Int),
            Member::Scalar("e", Scalar::Int),
            Member::Scalar("f", Scalar::Int),
        ],
    },
    Aggregate {
        name: "nested",
        kind: Kind::Struct,
        why: "A struct inside a struct with a float after it. Flattening is what every \
              classification rule does first, so a rule that stops at the outer members gets \
              this one wrong and gets nothing else wrong.",
        members: &[Member::Nested("a", 2), Member::Scalar("b", Scalar::Float)],
    },
    Aggregate {
        name: "int_or_float",
        kind: Kind::Union,
        why: "A union of an int and a float, which is one eightbyte with two classifications \
              and is why SysV's rule is a merge rather than a lookup.",
        members: &[Member::Scalar("a", Scalar::Int), Member::Scalar("b", Scalar::Float)],
    },
];

/// A type in the grammar, which is a scalar or one of the aggregates.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ty {
    Scalar(Scalar),
    Aggregate(usize),
}

impl Ty {
    /// What this becomes on the way past a `...`, which for an aggregate is itself.
    fn promoted(self) -> Ty {
        match self {
            Ty::Scalar(scalar) => Ty::Scalar(scalar.promoted()),
            Ty::Aggregate(_) => self,
        }
    }

    /// The C spelling, which for an aggregate is the tag and its keyword.
    fn spelling(self) -> String {
        match self {
            Ty::Scalar(scalar) => scalar.spelling().to_string(),
            Ty::Aggregate(at) => {
                let aggregate = &AGGREGATES[at];
                let keyword = match aggregate.kind {
                    Kind::Struct => "struct",
                    Kind::Union => "union",
                };
                format!("{keyword} {}", aggregate.name)
            }
        }
    }

    /// The scalars inside it, each with the path from the object to it.
    ///
    /// A union contributes its first member and no other, because a union holds one value at a
    /// time and the corpus has to know which one it put there. That is a real restriction on
    /// what this checks and it is the honest one: reading a member a program did not write is
    /// not a question about the ABI.
    fn leaves(self) -> Vec<(String, Scalar)> {
        match self {
            Ty::Scalar(scalar) => vec![(String::new(), scalar)],
            Ty::Aggregate(at) => {
                let aggregate = &AGGREGATES[at];
                let members: &[Member] = match aggregate.kind {
                    Kind::Struct => aggregate.members,
                    Kind::Union => &aggregate.members[..1],
                };
                let mut leaves = Vec::new();
                for member in members {
                    match member {
                        Member::Scalar(name, scalar) => {
                            leaves.push((format!(".{name}"), *scalar));
                        }
                        Member::Nested(name, inner) => {
                            for (path, scalar) in Ty::Aggregate(*inner).leaves() {
                                leaves.push((format!(".{name}{path}"), scalar));
                            }
                        }
                    }
                }
                leaves
            }
        }
    }

    /// An initializer for an object of this type holding `values`, taken in leaf order.
    fn initializer(self, values: &[String], next: &mut usize) -> String {
        match self {
            Ty::Scalar(_) => {
                let value = values[*next].clone();
                *next += 1;
                value
            }
            Ty::Aggregate(at) => {
                let aggregate = &AGGREGATES[at];
                let members: &[Member] = match aggregate.kind {
                    Kind::Struct => aggregate.members,
                    Kind::Union => &aggregate.members[..1],
                };
                let mut parts = Vec::new();
                for member in members {
                    let part = match member {
                        Member::Scalar(_, scalar) => Ty::Scalar(*scalar).initializer(values, next),
                        Member::Nested(_, inner) => Ty::Aggregate(*inner).initializer(values, next),
                    };
                    parts.push(part);
                }
                format!("{{ {} }}", parts.join(", "))
            }
        }
    }
}

/// One function in the corpus, with the values that travel through it.
struct Signature {
    name: String,
    /// Why it is here, written above the definition, and empty for a drawn one.
    why: &'static str,
    /// The return type, and [`None`] for a function returning `void`.
    ret: Option<Ty>,
    /// The value the callee returns and the caller checks, one per leaf of the return type.
    ret_values: Vec<String>,
    params: Vec<Param>,
    /// The arguments past the `...`, which is empty for a function that has no `...`.
    ///
    /// These have no parameters to sit on, which is the whole reason they are worth a corpus:
    /// nothing in the callee's declaration says where they are, so both sides work it out from
    /// the ABI alone and a disagreement has nothing to correct it. Each one's `ty` is what the
    /// callee reads it back as, which is the promoted type, and its values are written in the
    /// type the caller passed.
    varargs: Vec<Param>,
}

impl Signature {
    /// Whether the declaration ends in `...`.
    fn is_variadic(&self) -> bool {
        !self.varargs.is_empty()
    }
}

/// One parameter, with the values the caller passes and the callee checks.
struct Param {
    name: String,
    ty: Ty,
    values: Vec<String>,
}

/// The source of the values, which hands out a different one every time it is asked.
///
/// Every scalar in the program gets its own, so a value that arrives in the wrong place is a
/// value that belongs to some other parameter rather than a coincidence. The ranges are chosen so
/// that every value is exact in every type it can be written in: nothing that must fit a `long`
/// goes past thirty two bits, because a `long` is four bytes on Windows, and every floating point
/// value is a small number plus a quarter, which is exact in a `float` and in everything wider.
struct Values(u64);

impl Values {
    fn new() -> Values {
        Values(0)
    }

    fn next(&mut self, scalar: Scalar) -> String {
        self.0 += 1;
        let n = self.0;
        match scalar {
            Scalar::Char | Scalar::SChar => format!("({})({})", scalar.spelling(), 1 + n % 100),
            Scalar::UChar => format!("(unsigned char)({})", 1 + n % 240),
            Scalar::Short => format!("(short)({})", 1 + n % 30000),
            Scalar::UShort => format!("(unsigned short)({})", 1 + n % 60000),
            Scalar::Int => format!("{}", 1 + n * 7919 % 2_000_000_000),
            Scalar::UInt => format!("{}u", 1 + n * 7919 % 4_000_000_000),
            Scalar::Long => format!("{}L", 1 + n * 7919 % 2_000_000_000),
            Scalar::ULong => format!("{}UL", 1 + n * 7919 % 4_000_000_000),
            Scalar::LongLong => {
                format!("{}LL", 1 + n.wrapping_mul(0x0001_0f2c_3d4e_5f60) % 9_000_000_000_000_000)
            }
            Scalar::ULongLong => {
                format!("{}ULL", 1 + n.wrapping_mul(0x0001_0f2c_3d4e_5f60) % 18_000_000_000_000_000)
            }
            Scalar::Float => format!("{}.25f", 1 + n % 1000),
            Scalar::Double => format!("{}.25", 1 + n % 100_000),
            Scalar::LongDouble => format!("{}.25L", 1 + n % 100_000),
            Scalar::Pointer => format!("(void *)&anchor[{}]", n % ANCHOR),
        }
    }

    /// One value per leaf of `ty`.
    fn for_type(&mut self, ty: Ty) -> Vec<String> {
        ty.leaves().into_iter().map(|(_, scalar)| self.next(scalar)).collect()
    }
}

/// Write the corpus, or check that what is on disk still matches it.
pub(crate) fn run(root: &Path, mode: Mode) -> ExitCode {
    let signatures = signatures();
    let files = [
        ("abi.h", header(&signatures)),
        ("report.c", report()),
        ("callee.c", callee(&signatures)),
        ("caller.c", caller(&signatures)),
    ];
    let dir = root.join(DIR);

    if mode == Mode::Write {
        // No let chain here: this crate builds at the minimum supported Rust version, which
        // predates them, and the MSRV job is where a lapse gets found.
        if let Err(error) = std::fs::create_dir_all(&dir) {
            eprintln!("error: could not create {}: {error}", dir.display());
            return ExitCode::FAILURE;
        }
    }

    let mut stale = Vec::new();
    for (name, wanted) in &files {
        let path = dir.join(name);
        let found = std::fs::read_to_string(&path).ok();
        if found.as_deref() == Some(wanted.as_str()) {
            continue;
        }
        if mode == Mode::Check {
            stale.push(*name);
            continue;
        }
        if let Err(error) = std::fs::write(&path, wanted) {
            eprintln!("error: could not write {}: {error}", path.display());
            return ExitCode::FAILURE;
        }
    }

    if mode == Mode::Check {
        if stale.is_empty() {
            println!(
                "abi-signatures: {} functions in {} files are up to date",
                signatures.len(),
                files.len()
            );
            return ExitCode::SUCCESS;
        }
        println!(
            "abi-signatures: {} files are out of date, run `cargo xtask abi-signatures`",
            stale.len()
        );
        for name in stale {
            println!("  {DIR}/{name}");
        }
        return ExitCode::FAILURE;
    }
    println!("abi-signatures: wrote {} functions to {DIR}", signatures.len());
    ExitCode::SUCCESS
}

/// Every signature in the corpus, in declaration order.
///
/// Two halves, the same way the record corpus has two. The first is written by hand and every
/// entry is a case somebody named: the register file running out, the aggregate that goes to
/// memory, the return that comes back through a hidden pointer. The second is drawn from the
/// seed, for the orders nobody thinks to write down.
fn signatures() -> Vec<Signature> {
    let mut values = Values::new();
    let mut out = Vec::new();

    let mut named = |name: &str, why: &'static str, ret: Option<Ty>, params: Vec<Ty>| {
        out.push(build(&mut values, name.to_string(), why, ret, params));
    };

    named(
        "h_ints_past_the_registers",
        "Ten integers, which is more than any ABI here has argument registers, so the tail is on \
         the stack and the boundary between the two is what this is about.",
        Some(Ty::Scalar(Scalar::LongLong)),
        vec![Ty::Scalar(Scalar::LongLong); 10],
    );
    named(
        "h_doubles_past_the_registers",
        "Ten doubles, for the same reason and the other register file. SysV has eight of these \
         and Windows x64 has four, so the two disagree about where the fifth one is.",
        Some(Ty::Scalar(Scalar::Double)),
        vec![Ty::Scalar(Scalar::Double); 10],
    );
    named(
        "h_mixed_past_the_registers",
        "Integers and doubles alternating past the end of both files, which is where an ABI that \
         counts one register file decides differently from one that counts a slot per argument.",
        Some(Ty::Scalar(Scalar::Int)),
        vec![
            Ty::Scalar(Scalar::Int),
            Ty::Scalar(Scalar::Double),
            Ty::Scalar(Scalar::Int),
            Ty::Scalar(Scalar::Double),
            Ty::Scalar(Scalar::Int),
            Ty::Scalar(Scalar::Double),
            Ty::Scalar(Scalar::Int),
            Ty::Scalar(Scalar::Double),
            Ty::Scalar(Scalar::Int),
            Ty::Scalar(Scalar::Double),
            Ty::Scalar(Scalar::Int),
            Ty::Scalar(Scalar::Double),
        ],
    );
    named(
        "h_small_aggregates",
        "The aggregates that fit in registers, in one call, so the classification of each one is \
         checked with the register file already partly spent.",
        Some(Ty::Aggregate(2)),
        vec![Ty::Aggregate(0), Ty::Aggregate(1), Ty::Aggregate(2), Ty::Aggregate(3)],
    );
    named(
        "h_float_aggregates",
        "The homogeneous floating point aggregates, which are the ones AAPCS64 puts in vector \
         registers and SysV packs into SSE eightbytes.",
        Some(Ty::Aggregate(5)),
        vec![Ty::Aggregate(4), Ty::Aggregate(5), Ty::Aggregate(6)],
    );
    named(
        "h_memory_aggregate",
        "The aggregate that is too large for registers, with an integer either side of it, so a \
         caller that forgets it left a copy behind puts the next argument in the wrong place.",
        Some(Ty::Aggregate(9)),
        vec![Ty::Scalar(Scalar::Int), Ty::Aggregate(9), Ty::Scalar(Scalar::Int)],
    );
    named(
        "h_returns_by_hidden_pointer",
        "A large aggregate returned, which every ABI here does by giving the callee the address \
         to write it to. That address is an argument nobody wrote, so it moves every other one.",
        Some(Ty::Aggregate(9)),
        vec![Ty::Scalar(Scalar::Int), Ty::Scalar(Scalar::Double)],
    );
    named(
        "h_returns_nothing",
        "A function returning void with arguments that fill the registers, because the return \
         value is what an ABI spends a register on before the arguments and this is the case \
         where it does not.",
        None,
        vec![
            Ty::Aggregate(2),
            Ty::Scalar(Scalar::Double),
            Ty::Scalar(Scalar::LongDouble),
            Ty::Scalar(Scalar::Pointer),
        ],
    );
    named(
        "h_long_double_and_friends",
        "A long double between two integers, which is the type whose size, alignment and format \
         all move between rows of the table.",
        Some(Ty::Scalar(Scalar::LongDouble)),
        vec![Ty::Scalar(Scalar::Int), Ty::Scalar(Scalar::LongDouble), Ty::Scalar(Scalar::Int)],
    );
    named(
        "h_nested_and_union",
        "The nested aggregate and the union, which are the two shapes a classification rule has \
         to flatten before it can decide anything.",
        Some(Ty::Aggregate(11)),
        vec![Ty::Aggregate(10), Ty::Aggregate(11), Ty::Aggregate(8)],
    );

    let mut rng = Rng::new(SEED);
    for index in 0..DRAWN {
        let count = rng.below(9) as usize;
        let params: Vec<Ty> = (0..count).map(|_| draw(&mut rng)).collect();
        // A void return one time in eight. Everything else returns something, because the return
        // value is half of what this corpus is checking.
        let ret = if rng.below(8) == 0 { None } else { Some(draw(&mut rng)) };
        out.push(build(&mut values, format!("g{index:02}"), "", ret, params));
    }

    let mut variadic =
        |name: &str, why: &'static str, ret: Option<Ty>, params: Vec<Ty>, varargs: Vec<Ty>| {
            out.push(build_variadic(&mut values, name.to_string(), why, ret, params, varargs));
        };

    variadic(
        "hv_ints_past_the_registers",
        "Ten integers past the dots, which is more than any ABI here has argument registers, so \
         the callee reads some of them out of a register save area and the rest off the stack. \
         Where those two meet is the thing va_arg is easiest to get wrong about.",
        Some(Ty::Scalar(Scalar::LongLong)),
        vec![Ty::Scalar(Scalar::Int)],
        vec![Ty::Scalar(Scalar::LongLong); 10],
    );
    variadic(
        "hv_doubles_past_the_registers",
        "The same for the other register file, which on SysV is a second save area with a count \
         of its own, and on Windows x64 is the general purpose registers because a variadic call \
         there puts a double in both.",
        Some(Ty::Scalar(Scalar::Double)),
        vec![Ty::Scalar(Scalar::Int)],
        vec![Ty::Scalar(Scalar::Double); 10],
    );
    variadic(
        "hv_mixed_past_the_registers",
        "Integers and doubles alternating past the dots, which is the case where the two save \
         areas are being walked at once and each one has its own idea of how far along it is.",
        Some(Ty::Scalar(Scalar::Int)),
        vec![Ty::Scalar(Scalar::Int)],
        vec![
            Ty::Scalar(Scalar::Int),
            Ty::Scalar(Scalar::Double),
            Ty::Scalar(Scalar::Int),
            Ty::Scalar(Scalar::Double),
            Ty::Scalar(Scalar::Int),
            Ty::Scalar(Scalar::Double),
            Ty::Scalar(Scalar::Int),
            Ty::Scalar(Scalar::Double),
            Ty::Scalar(Scalar::Int),
            Ty::Scalar(Scalar::Double),
            Ty::Scalar(Scalar::Int),
            Ty::Scalar(Scalar::Double),
        ],
    );
    variadic(
        "hv_promotions",
        "The six types no program can pass through a `...`, passed anyway. Everything narrower \
         than an int arrives as an int and a float arrives as a double, so the callee reads back \
         a type the caller never wrote, and a compiler that skipped the promotion puts two bytes \
         where four are read.",
        Some(Ty::Scalar(Scalar::Int)),
        vec![Ty::Scalar(Scalar::Int)],
        vec![
            Ty::Scalar(Scalar::Char),
            Ty::Scalar(Scalar::SChar),
            Ty::Scalar(Scalar::UChar),
            Ty::Scalar(Scalar::Short),
            Ty::Scalar(Scalar::UShort),
            Ty::Scalar(Scalar::Float),
        ],
    );
    variadic(
        "hv_float_aggregates",
        "The homogeneous floating point aggregates past the dots, which is the one place the five \
         ABIs described here do not all answer the same way. Darwin arm64 puts every variadic \
         argument in the argument area, so the two floats AAPCS64 would give two vector registers \
         are on the stack, and a caller that asked the fixed question writes registers the callee \
         never reads.",
        Some(Ty::Aggregate(5)),
        vec![Ty::Scalar(Scalar::Int)],
        vec![Ty::Aggregate(4), Ty::Aggregate(5), Ty::Aggregate(6)],
    );
    variadic(
        "hv_small_aggregates",
        "The aggregates that fit in registers, past the dots, where the question is whether the \
         classification that put them there is the same classification the callee undoes.",
        Some(Ty::Aggregate(2)),
        vec![Ty::Scalar(Scalar::Int)],
        vec![Ty::Aggregate(0), Ty::Aggregate(1), Ty::Aggregate(2), Ty::Aggregate(3)],
    );
    variadic(
        "hv_memory_aggregate",
        "The aggregate too large for registers, past the dots, with something either side of it, \
         so a callee that walks past the wrong number of bytes reads the next argument.",
        Some(Ty::Scalar(Scalar::Int)),
        vec![Ty::Scalar(Scalar::Int)],
        vec![Ty::Scalar(Scalar::Int), Ty::Aggregate(9), Ty::Scalar(Scalar::Int)],
    );
    variadic(
        "hv_returns_by_hidden_pointer",
        "A variadic function returning a large aggregate, which is the two argument shifting \
         rules at once: the hidden pointer takes a register before anything else, and everything \
         past the dots is placed after that.",
        Some(Ty::Aggregate(9)),
        vec![Ty::Scalar(Scalar::Int)],
        vec![Ty::Scalar(Scalar::Double), Ty::Scalar(Scalar::LongLong)],
    );
    variadic(
        "hv_long_double_and_friends",
        "A long double past the dots between two integers, which is the type whose size, \
         alignment and format all move between rows, and which the save area has to be aligned \
         for wherever it is sixteen bytes.",
        None,
        vec![Ty::Scalar(Scalar::Int)],
        vec![
            Ty::Scalar(Scalar::Int),
            Ty::Scalar(Scalar::LongDouble),
            Ty::Scalar(Scalar::Int),
            Ty::Scalar(Scalar::LongDouble),
        ],
    );
    variadic(
        "hv_registers_already_spent",
        "Named parameters that use up the registers before the dots are reached, so every \
         variadic argument is on the stack and the save area holds nothing the callee wants. The \
         opposite of the case above it, and the one where an off by one in the save area offset \
         does not show up.",
        Some(Ty::Scalar(Scalar::Int)),
        vec![Ty::Scalar(Scalar::LongLong); 8],
        vec![Ty::Scalar(Scalar::LongLong), Ty::Scalar(Scalar::Double), Ty::Aggregate(2)],
    );

    for index in 0..DRAWN_VARIADIC {
        // At least one named parameter, because `va_start` needs one to name.
        let named = 1 + rng.below(3) as usize;
        let mut params: Vec<Ty> = (0..named).map(|_| draw(&mut rng)).collect();
        // C17 7.16.1.4p4: the parameter `va_start` names must not be one the default argument
        // promotions would change, and the behaviour is undefined rather than diagnosed, which is
        // the kind of rule a generator walks straight into. A drawn `short` becomes the `int` it
        // would have been promoted to instead of being redrawn, so the seed still spends the same
        // number of draws and the corpus does not move when this line changes.
        let last = params.len() - 1;
        params[last] = params[last].promoted();
        // At least one argument past the dots, because a variadic function nothing is passed to
        // is an ordinary function with a comma in it.
        let count = 1 + rng.below(6) as usize;
        let varargs: Vec<Ty> = (0..count).map(|_| draw(&mut rng)).collect();
        let ret = if rng.below(8) == 0 { None } else { Some(draw(&mut rng)) };
        out.push(build_variadic(&mut values, format!("v{index:02}"), "", ret, params, varargs));
    }
    out
}

/// One type, drawn from the seed.
///
/// An aggregate one time in three, because a corpus of nothing but aggregates would never run
/// the register file out with scalars and a corpus of nothing but scalars would check the easy
/// half of every ABI.
fn draw(rng: &mut Rng) -> Ty {
    if rng.below(3) == 0 {
        Ty::Aggregate(rng.below(AGGREGATES.len() as u64) as usize)
    } else {
        Ty::Scalar(SCALARS[rng.below(SCALARS.len() as u64) as usize])
    }
}

/// A signature with its values drawn.
fn build(
    values: &mut Values,
    name: String,
    why: &'static str,
    ret: Option<Ty>,
    params: Vec<Ty>,
) -> Signature {
    build_variadic(values, name, why, ret, params, Vec::new())
}

/// The same, with arguments past a `...`.
///
/// The values of a variadic argument are drawn from the type the caller writes and the parameter
/// carries the type the callee reads, which are the same thing for everything the default
/// argument promotions leave alone and are not for the six types they do not.
fn build_variadic(
    values: &mut Values,
    name: String,
    why: &'static str,
    ret: Option<Ty>,
    params: Vec<Ty>,
    varargs: Vec<Ty>,
) -> Signature {
    let params: Vec<Param> = params
        .into_iter()
        .enumerate()
        .map(|(index, ty)| Param { name: format!("a{index}"), values: values.for_type(ty), ty })
        .collect();
    let varargs = varargs
        .into_iter()
        .enumerate()
        .map(|(index, ty)| Param {
            name: format!("v{index}"),
            values: values.for_type(ty),
            ty: ty.promoted(),
        })
        .collect();
    let ret_values = match ret {
        Some(ty) => values.for_type(ty),
        None => Vec::new(),
    };
    Signature { name, why, ret, ret_values, params, varargs }
}

/// The prototype of a signature, without the trailing semicolon.
fn prototype(signature: &Signature) -> String {
    let mut params = if signature.params.is_empty() {
        "void".to_string()
    } else {
        signature
            .params
            .iter()
            .map(|param| declarator(param.ty, &param.name))
            .collect::<Vec<_>>()
            .join(", ")
    };
    // C17 6.7.6.3p4 wants at least one named parameter in front of the `...`, and every variadic
    // signature here has one because `va_start` needs something to name.
    if signature.is_variadic() {
        params.push_str(", ...");
    }
    match signature.ret {
        Some(ty) => declarator(ty, &format!("{}({params})", signature.name)),
        None => format!("void {}({params})", signature.name),
    }
}

/// A declaration of `name` with that type, which is not a concatenation for a pointer.
fn declarator(ty: Ty, name: &str) -> String {
    match ty {
        Ty::Scalar(Scalar::Pointer) => format!("void *{name}"),
        _ => format!("{} {name}", ty.spelling()),
    }
}

/// The comment every generated file starts with.
fn banner(out: &mut String, what: &str) {
    let _ = writeln!(out, "/* {what}");
    for line in [
        "",
        "Generated by `cargo run -q -p rucc-targets -- abi-signatures --write`. Do not edit",
        "this file, edit the grammar in `build-tools/rucc-targets/src/signatures.rs`.",
        "",
        "This is the differential ABI harness of `spec/cross-compile/14-testing.md` section",
        "14.3. `callee.c` is compiled by one compiler and `caller.c` by the other, the two are",
        "linked together and the program is run, so a value that arrives wrong is the two",
        "compilers disagreeing about a calling convention rather than about a layout.",
        "",
        "The program prints nothing and exits zero when the two agree. Every disagreement",
        "prints the function and the parameter it is about, and the exit status is one.",
    ] {
        if line.is_empty() {
            out.push_str(" *\n");
        } else {
            let _ = writeln!(out, " * {line}");
        }
    }
    out.push_str(" */\n\n");
}

/// A comment holding `why`, wrapped, or nothing at all when there is no reason to give.
fn reason(out: &mut String, why: &str) {
    if why.is_empty() {
        return;
    }
    out.push_str("/* ");
    let mut column = 3;
    for word in why.split_whitespace() {
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

/// `abi.h`, which is the only thing the two sides share.
fn header(signatures: &[Signature]) -> String {
    let mut out = String::new();
    banner(&mut out, "The types and the prototypes both sides of the differential agree on.");
    out.push_str("#ifndef RUCC_ABI_SIGNATURES_H\n#define RUCC_ABI_SIGNATURES_H\n\n");

    for aggregate in AGGREGATES {
        reason(&mut out, aggregate.why);
        let keyword = match aggregate.kind {
            Kind::Struct => "struct",
            Kind::Union => "union",
        };
        let _ = writeln!(out, "{keyword} {} {{", aggregate.name);
        for member in aggregate.members {
            match member {
                Member::Scalar(name, scalar) => {
                    let _ = writeln!(out, "\t{};", declarator(Ty::Scalar(*scalar), name));
                }
                Member::Nested(name, inner) => {
                    let _ = writeln!(out, "\t{};", declarator(Ty::Aggregate(*inner), name));
                }
            }
        }
        out.push_str("};\n\n");
    }

    out.push_str(
        "/* The bytes a pointer argument points into, defined in report.c. Nothing reads them:\n\
         \x20* what travels is the address, and an address inside a known object is one both\n\
         \x20* sides can name without either of them having to agree about a number. */\n",
    );
    let _ = writeln!(out, "extern char anchor[{ANCHOR}];\n");

    out.push_str(
        "/* How many disagreements have been seen, and how one is recorded. Both live in\n\
         \x20* report.c, which is the only file here that includes a libc header and is always\n\
         \x20* built by the reference compiler. Two `const char *` and a `void` return is the one\n\
         \x20* signature every ABI in the table agrees about, so calling this is not itself a\n\
         \x20* thing the corpus can get wrong. */\n",
    );
    out.push_str("extern int abi_failures;\n");
    out.push_str("void abi_fail(const char *fn, const char *slot);\n\n");

    out.push_str(
        "/* Reading the arguments past a `...`, spelled with the builtins rather than with\n\
         \x20* <stdarg.h>. The two files under test include no libc header, for the reason\n\
         \x20* report.c exists, and stdarg.h is the one header a freestanding program is still\n\
         \x20* allowed to want. Every compiler this corpus is compiled by implements va_start,\n\
         \x20* va_arg and va_end as exactly these builtins, so this is the same header with one\n\
         \x20* fewer thing that has to be found on disk. */\n",
    );
    out.push_str("#define ABI_VA_LIST __builtin_va_list\n");
    out.push_str("#define ABI_VA_START(ap, last) __builtin_va_start(ap, last)\n");
    out.push_str("#define ABI_VA_ARG(ap, ty) __builtin_va_arg(ap, ty)\n");
    out.push_str("#define ABI_VA_END(ap) __builtin_va_end(ap)\n\n");

    for signature in signatures {
        reason(&mut out, signature.why);
        let _ = writeln!(out, "{};", prototype(signature));
        if !signature.why.is_empty() {
            out.push('\n');
        }
    }

    out.push_str("\n#endif\n");
    out
}

/// `report.c`, which holds the two data objects and the one function that prints.
///
/// Separate from the other two so that neither of them has to include a header. It is always
/// compiled by the reference compiler, in every direction the harness runs, which is why it is
/// the file the libc call lives in.
fn report() -> String {
    let mut out = String::new();
    banner(&mut out, "The failure counter, and the only libc call in the corpus.");
    out.push_str("#include <stdio.h>\n\n");
    out.push_str("#include \"abi.h\"\n\n");
    let _ = writeln!(out, "char anchor[{ANCHOR}];");
    out.push_str("int abi_failures;\n\n");
    out.push_str("void abi_fail(const char *fn, const char *slot)\n{\n");
    out.push_str("\tabi_failures++;\n");
    out.push_str("\tfprintf(stderr, \"%s: %s did not arrive\\n\", fn, slot);\n");
    out.push_str("}\n");
    out
}

/// `callee.c`, which is every definition and every check of an argument.
fn callee(signatures: &[Signature]) -> String {
    let mut out = String::new();
    banner(&mut out, "The definitions, which check what arrived and return what is expected.");
    out.push_str("#include \"abi.h\"\n\n");

    for signature in signatures {
        let _ = writeln!(out, "{}\n{{", prototype(signature));
        if signature.is_variadic() {
            out.push_str("\tABI_VA_LIST ap;\n\n");
        }
        for param in &signature.params {
            for ((path, _), value) in param.ty.leaves().iter().zip(&param.values) {
                let slot = format!("{}{path}", param.name);
                let _ = writeln!(
                    out,
                    "\tif ({slot} != {value})\n\t\tabi_fail(\"{}\", \"{slot}\");",
                    signature.name
                );
            }
        }
        if signature.is_variadic() {
            let last = signature.params.last().expect("a variadic signature has a named parameter");
            let _ = writeln!(out, "\n\tABI_VA_START(ap, {});", last.name);
            for param in &signature.varargs {
                // Each one in a block of its own, so the declaration is beside the read and a
                // corpus compiled at -std=c89 one day would still be one declaration per block.
                out.push_str("\t{\n");
                let _ = writeln!(
                    out,
                    "\t\t{} = ABI_VA_ARG(ap, {});",
                    declarator(param.ty, &param.name),
                    param.ty.spelling()
                );
                for ((path, _), value) in param.ty.leaves().iter().zip(&param.values) {
                    let slot = format!("{}{path}", param.name);
                    let _ = writeln!(
                        out,
                        "\t\tif ({slot} != {value})\n\t\t\tabi_fail(\"{}\", \"{slot}\");",
                        signature.name
                    );
                }
                out.push_str("\t}\n");
            }
            out.push_str("\tABI_VA_END(ap);\n\n");
        }
        if let Some(ty) = signature.ret {
            let mut next = 0;
            let initializer = ty.initializer(&signature.ret_values, &mut next);
            match ty {
                Ty::Scalar(_) => {
                    let _ = writeln!(out, "\treturn {initializer};");
                }
                Ty::Aggregate(_) => {
                    let _ = writeln!(out, "\t{} = {initializer};", declarator(ty, "r"));
                    out.push_str("\treturn r;\n");
                }
            }
        }
        out.push_str("}\n\n");
    }
    out
}

/// `caller.c`, which is `main`, every call and every check of a return value.
fn caller(signatures: &[Signature]) -> String {
    let mut out = String::new();
    banner(&mut out, "The calls, which pass what the callee expects and check what came back.");
    out.push_str("#include \"abi.h\"\n\n");
    out.push_str("int main(void)\n{\n");

    for signature in signatures {
        out.push_str("\t{\n");
        let mut arguments = Vec::new();
        for param in &signature.params {
            match param.ty {
                Ty::Scalar(_) => arguments.push(param.values[0].clone()),
                Ty::Aggregate(_) => {
                    let mut next = 0;
                    let initializer = param.ty.initializer(&param.values, &mut next);
                    let _ =
                        writeln!(out, "\t\t{} = {initializer};", declarator(param.ty, &param.name));
                    arguments.push(param.name.clone());
                }
            }
        }
        for param in &signature.varargs {
            match param.ty {
                // A scalar goes in as the literal it is. The default argument promotions widen
                // the literal exactly as they would widen a variable of the type it was written
                // in, so there is nothing a local would add except a line.
                Ty::Scalar(_) => arguments.push(param.values[0].clone()),
                Ty::Aggregate(_) => {
                    let mut next = 0;
                    let initializer = param.ty.initializer(&param.values, &mut next);
                    let _ =
                        writeln!(out, "\t\t{} = {initializer};", declarator(param.ty, &param.name));
                    arguments.push(param.name.clone());
                }
            }
        }
        let call = format!("{}({})", signature.name, arguments.join(", "));
        match signature.ret {
            None => {
                let _ = writeln!(out, "\t\t{call};");
            }
            Some(ty) => {
                let _ = writeln!(out, "\t\t{} = {call};", declarator(ty, "r"));
                for ((path, _), value) in ty.leaves().iter().zip(&signature.ret_values) {
                    let _ = writeln!(
                        out,
                        "\t\tif (r{path} != {value})\n\t\t\tabi_fail(\"{}\", \"return{path}\");",
                        signature.name
                    );
                }
            }
        }
        out.push_str("\t}\n");
    }

    out.push_str("\treturn abi_failures == 0 ? 0 : 1;\n}\n");
    out
}
