//! The GNU compatibility surface: features.toml, attributes, builtins, pragmas.
//!
//! Design: `spec/13-gnu-compat.md`. Layer rank 5, see `spec/18-package-layout.md`.
//!
//! # Status
//!
//! The matrix is real. `features.toml` next to this file is the source of truth for what the
//! compiler claims to support, `build.rs` turns it into the table below, and the `__has_*`
//! family in the preprocessor answers out of it. The attributes and builtins themselves land
//! with the parser, and every row that says `unimplemented` says so because it is.
//!
//! The rule that makes the table worth having is in section 13.2: answering `__has_builtin`
//! untruthfully is worse than answering no, because a header that gets a yes and then fails
//! to compile is much harder to diagnose than one that takes its fallback path. So only a row
//! marked `implemented` answers yes, and a row marked `implemented` with no test named
//! against it fails the build.
//!
//! ```
//! use rucc_gnu::{Kind, Status, Target};
//!
//! let linux = Target::new("x86_64", "linux");
//! assert_eq!(rucc_gnu::has_feature("__has_include", linux), 1);
//! assert_eq!(rucc_gnu::has_attribute("cleanup", linux), 1);
//! assert_eq!(rucc_gnu::has_attribute("transparent_union", linux), 1);
//! assert_eq!(rucc_gnu::has_attribute("no_such_attribute", linux), 0);
//!
//! // Some answers are the target's. gcc has the 32-bit conventions on every x86 target and
//! // the DLL attributes only where there are DLLs.
//! assert_eq!(rucc_gnu::has_attribute("stdcall", linux), 1);
//! assert_eq!(rucc_gnu::has_attribute("dllimport", linux), 0);
//! assert_eq!(rucc_gnu::has_attribute("dllimport", Target::new("x86_64", "windows")), 1);
//! assert_eq!(rucc_gnu::has_attribute("stdcall", Target::new("aarch64", "linux")), 0);
//!
//! // The armoured spelling is the same question.
//! assert_eq!(rucc_gnu::lookup(Kind::Attribute, "__packed__").map(|f| f.name), Some("packed"));
//!
//! // Nested functions are done, and the operators still answer what gcc answers for the name.
//! let nested = rucc_gnu::lookup(Kind::Extension, "nested_functions").unwrap();
//! assert_eq!(nested.status, Status::Implemented);
//! ```
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.

#![doc(html_root_url = "https://docs.rs/rucc-gnu/0.18.12")]

/// What kind of thing a row of the matrix describes.
///
/// The kind is part of the identity of a row, because `deprecated` is both a GNU attribute
/// and a C23 one and the two are answered by different operators with different values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// `__attribute__((x))` and `[[gnu::x]]`, asked about with `__has_attribute`.
    Attribute,
    /// A standard `[[x]]` attribute, asked about with `__has_c_attribute`.
    CAttribute,
    /// `__builtin_x`, asked about with `__has_builtin`.
    Builtin,
    /// A language or preprocessor feature, asked about with `__has_feature`.
    Feature,
    /// A GNU extension to the language, asked about with `__has_extension`.
    Extension,
}

/// How far along a row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Status {
    /// Recognised and not done. The `__has_*` operators answer no.
    Unimplemented,
    /// Some of it works. The `__has_*` operators still answer no, because a feature that
    /// works most of the time is exactly the case where the fallback path is the safer one.
    Partial,
    /// Done, with a test named against it.
    Implemented,
    /// Will not be done, and the row says why.
    Rejected,
}

impl Status {
    /// Whether the `__has_*` family answers yes for a row at this status.
    pub const fn is_available(self) -> bool {
        matches!(self, Status::Implemented)
    }
}

/// What happens when the compiler meets something this row describes and cannot do it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Answer {
    /// Warn and carry on, which is what GCC does for an attribute it does not know. Ignoring
    /// `hot` produces slower code and nothing worse.
    Warn,
    /// Refuse. Ignoring `packed`, `aligned`, `section`, `no_sanitize` or `naked` produces
    /// wrong code rather than slow code, and wrong code that compiles is the worst outcome
    /// available. This is section 13.4's rule.
    Error,
}

/// A kind of target a row can be limited to.
///
/// GCC's answer to `__has_attribute(stdcall)` is one on x86 and zero on AArch64, and its answer
/// to `__has_attribute(dllimport)` is one where there are DLLs and zero anywhere else, because
/// each target back end brings its own attributes with it. A row whose answer is like that names
/// the places it is there, and answers zero anywhere else whatever its status says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Place {
    /// 64-bit x86, on any operating system.
    X86_64,
    /// 32-bit x86, on any operating system.
    X86,
    /// 64-bit Arm, on any operating system.
    Aarch64,
    /// Windows, on any architecture.
    Windows,
    /// 64-bit x86 with ELF objects, which is every operating system but Windows and macOS, and
    /// the one place indirect functions are built.
    X86_64Elf,
}

/// The target a question is asked on, as far as the matrix cares, which is which of the places
/// a row can name hold for it.
///
/// The default is a target that is none of them, where a row limited to some places answers
/// zero and every other row answers what its status says.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Target {
    places: u8,
}

impl Target {
    /// The target with this architecture and operating system, spelled the way a target tuple
    /// spells them, so `x86_64` or `i686` and `linux` or `windows`.
    pub fn new(arch: &str, os: &str) -> Target {
        let mut target = Target::default();
        for (place, holds) in [
            (Place::X86_64, arch == "x86_64"),
            (Place::X86, arch == "i686"),
            (Place::Aarch64, arch == "aarch64"),
            (Place::Windows, os == "windows"),
            (Place::X86_64Elf, arch == "x86_64" && !matches!(os, "windows" | "macos")),
        ] {
            if holds {
                target.places |= 1 << place as u8;
            }
        }
        target
    }

    /// Whether this target is one of those.
    pub fn is(self, place: Place) -> bool {
        self.places & (1 << place as u8) != 0
    }
}

/// One row of the matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Feature {
    /// The spelling asked about, with no `__` armour on it.
    pub name: &'static str,
    /// Which operator answers for it.
    pub kind: Kind,
    /// The GCC release that introduced it.
    pub gcc_version: &'static str,
    /// How far along it is.
    pub status: Status,
    /// The type a builtin has, written as a C prototype without the name, or empty.
    ///
    /// Empty for everything that is not a builtin, and for a builtin whose type depends on
    /// what it is handed: `__builtin_constant_p` takes anything, `__builtin_add_overflow`
    /// takes three types that have to agree, and the atomics are a family rather than a
    /// function. Those are decided where the arguments are, and a fixed type here would be a
    /// worse answer than none.
    ///
    /// It is a string rather than a structure because `size_t` is a different type on two
    /// targets and this table has no target. The compiler reads it once per builtin it is
    /// asked for. The set of words it may use is fixed and `build.rs` checks it, so a typo
    /// fails this crate's build rather than the compile of whoever first calls the builtin.
    pub signature: &'static str,
    /// The library function this builtin is, for the family where that is the whole answer.
    ///
    /// Empty for everything else. GCC's `__builtin_abort` is a call to `abort`, its
    /// `__builtin_strlen` a call to `strlen`, and the prefix is there so that a program can
    /// reach the function the C library promises even where its own name has been taken by a
    /// macro or by a definition of its own. GCC folds some of these when the arguments allow
    /// it, and folding is an optimization on top: the call is the meaning, and a compiler that
    /// only ever emits the call is right and slow rather than wrong.
    ///
    /// The name is written out rather than worked out by stripping the prefix, because the two
    /// are the same for every row here and need not be for the next one, and a table that says
    /// what it means is worth more than one that saves thirty words.
    pub library: &'static str,
    /// What to do when it is met and is not implemented.
    pub answer: Answer,
    /// What `__has_c_attribute` answers with, which the standard fixes per attribute. One for
    /// every other kind, where the operators answer one or nothing, except on a row that says
    /// zero.
    ///
    /// Zero is how a row says that gcc 16 does not know the name. `__has_extension(case_ranges)`
    /// is the example: this compiler has case ranges and gcc has them too, and gcc still answers
    /// no, because the names `__has_extension` knows are clang's and this is not one of them. A
    /// program that asks is asking a question written for clang or for this table, and the answer
    /// it gets from gcc is the one it has to be able to live with, so the row keeps its status,
    /// which says what the compiler does, and answers what gcc answers.
    pub value: u32,
    /// The places the row answers on, or none for a row that answers the same everywhere.
    ///
    /// On a target that is none of them the `__has_*` operators answer zero, whatever the status
    /// says, which is how a row says that gcc 16 only knows the name on some targets. The status
    /// is then about those targets alone, so an attribute that is done on x86-64 and not yet on
    /// 32-bit x86 names x86-64 and is implemented.
    pub targets: &'static [Place],
    /// Projects known to need it, from the corpus in `spec/15-testing.md`.
    pub used_by: &'static [&'static str],
    /// The tests that prove the status, named as `crate::test` or as a file path.
    pub tests: &'static [&'static str],
    /// Anything a reader needs that the fields above do not say.
    pub notes: &'static str,
}

impl Feature {
    /// Whether the row is there at all on this target, which it is everywhere unless it names
    /// the places it is limited to.
    pub fn is_on(&self, target: Target) -> bool {
        self.targets.is_empty() || self.targets.iter().any(|&place| target.is(place))
    }
}

include!(concat!(env!("OUT_DIR"), "/features.rs"));

/// The whole matrix, sorted by kind and then by name.
pub fn features() -> &'static [Feature] {
    FEATURES
}

/// The row for a name, if the matrix has one.
///
/// The `__x__` spelling is the same question as `x`, because that is how a header writes an
/// attribute name that a macro might otherwise have taken.
pub fn lookup(kind: Kind, name: &str) -> Option<&'static Feature> {
    let bare = unarmour(name);
    let at = FEATURES.binary_search_by(|f| f.kind.cmp(&kind).then_with(|| f.name.cmp(bare)));
    at.ok().map(|at| &FEATURES[at])
}

/// What `__has_attribute(name)` answers.
///
/// A name the standard also has is answered with the standard's number, as GCC answers it, so
/// `__has_attribute(fallthrough)` is 202311 rather than one. GCC does that whether or not the name
/// is also a GNU attribute, so `maybe_unused`, which is only a standard one, is answered too.
pub fn has_attribute(name: &str, target: Target) -> u32 {
    match answer(Kind::CAttribute, name, target) {
        0 => answer(Kind::Attribute, name, target),
        standard => standard,
    }
}

/// What `__has_attribute(gnu::name)` and `__has_c_attribute(gnu::name)` answer.
///
/// A scoped name is asked of the GNU attributes alone, so the answer is one when the matrix has
/// the name as a GNU attribute rucc has, and zero otherwise, even for a name the standard also
/// has. That is gcc 16's answer: `gnu::fallthrough` is one rather than 202311, and
/// `gnu::nodiscard`, which GCC only has as a standard attribute, is zero.
pub fn has_gnu_attribute(name: &str, target: Target) -> u32 {
    u32::from(answer(Kind::Attribute, name, target) != 0)
}

/// What `__has_c_attribute(name)` answers, which is the number the standard gives the
/// attribute rather than one.
pub fn has_c_attribute(name: &str, target: Target) -> u32 {
    answer(Kind::CAttribute, name, target)
}

/// What `__has_builtin(name)` answers.
pub fn has_builtin(name: &str, target: Target) -> u32 {
    answer(Kind::Builtin, name, target)
}

/// What `__has_feature(name)` answers.
pub fn has_feature(name: &str, target: Target) -> u32 {
    answer(Kind::Feature, name, target)
}

/// What `__has_extension(name)` answers.
///
/// GCC treats the two as the same question and so do we: a feature that is available is
/// available whether or not the mode it is asked in makes it standard.
pub fn has_extension(name: &str, target: Target) -> u32 {
    let extension = answer(Kind::Extension, name, target);
    if extension == 0 { answer(Kind::Feature, name, target) } else { extension }
}

fn answer(kind: Kind, name: &str, target: Target) -> u32 {
    match lookup(kind, name) {
        Some(feature) if feature.status.is_available() && feature.is_on(target) => feature.value,
        _ => 0,
    }
}

/// `__packed__` and `packed` are the same attribute.
///
/// This is public because [`lookup`] is not the only thing that has to know it. Anything that
/// reads a name out of an attribute list and compares it against a spelling has the same
/// question, and a header writes the armoured form precisely so that a program's own macro
/// called `packed` cannot take the plain one, so a compiler that only knows the plain one reads
/// the wrong layout out of a header that was careful.
#[must_use]
pub fn unarmour(name: &str) -> &str {
    let bare = name.strip_prefix("__").and_then(|n| n.strip_suffix("__"));
    match bare {
        // `__builtin_x` and the atomics keep their prefix, because it is part of the name
        // rather than armour around it.
        Some(bare) if !bare.is_empty() && !name.starts_with("__builtin") => bare,
        _ => name,
    }
}

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M1";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_sorted_so_the_lookup_can_be_a_search() {
        let keys: Vec<(Kind, &str)> = FEATURES.iter().map(|f| (f.kind, f.name)).collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(keys, sorted);
    }

    const LINUX: Target = Target { places: 1 << Place::X86_64 as u8 | 1 << Place::X86_64Elf as u8 };

    #[test]
    fn every_row_is_findable_by_its_own_name() {
        for feature in FEATURES {
            assert_eq!(lookup(feature.kind, feature.name), Some(feature));
        }
    }

    #[test]
    fn a_name_that_is_not_in_the_matrix_answers_no() {
        assert_eq!(has_attribute("nonesuch", LINUX), 0);
        assert_eq!(has_builtin("__builtin_nonesuch", LINUX), 0);
        assert_eq!(has_feature("nonesuch", LINUX), 0);
        assert_eq!(lookup(Kind::Attribute, "nonesuch"), None);
    }

    #[test]
    fn the_armoured_spelling_is_the_same_question() {
        assert_eq!(lookup(Kind::Attribute, "__packed__").map(|f| f.name), Some("packed"));
        assert_eq!(lookup(Kind::Attribute, "packed").map(|f| f.name), Some("packed"));
        assert_eq!(lookup(Kind::Attribute, "__packed"), None, "half the armour is not a name");
    }

    #[test]
    fn a_builtin_keeps_the_prefix_that_is_part_of_its_name() {
        assert!(lookup(Kind::Builtin, "__builtin_expect").is_some());
        assert_eq!(lookup(Kind::Builtin, "expect"), None);
    }

    #[test]
    fn only_an_implemented_row_answers_yes() {
        for feature in FEATURES {
            let answered = answer(feature.kind, feature.name, LINUX);
            assert_eq!(
                answered != 0,
                feature.status == Status::Implemented && feature.value != 0 && feature.is_on(LINUX),
                "{} answered {answered} at status {:?}",
                feature.name,
                feature.status
            );
        }
    }

    /// A row that answers no while saying it is implemented is one gcc 16 answers no for, and it
    /// is never an attribute, since gcc knows every attribute the table has a row for.
    #[test]
    fn a_row_answers_no_on_purpose_only_where_gcc_does() {
        for feature in FEATURES {
            if feature.value == 0 {
                assert!(
                    matches!(feature.kind, Kind::Extension | Kind::Feature | Kind::Builtin),
                    "{} is an attribute gcc knows",
                    feature.name
                );
            }
        }
        let ranges = lookup(Kind::Extension, "case_ranges").expect("in the table");
        assert_eq!(ranges.status, Status::Implemented);
        assert_eq!(has_extension("case_ranges", LINUX), 0, "gcc 16 does not know the name");
    }

    #[test]
    fn an_implemented_row_names_a_test() {
        // build.rs enforces this too. It is here as well because the build script failing is
        // a harder message to read than a failing test.
        for feature in FEATURES {
            if feature.status == Status::Implemented {
                assert!(!feature.tests.is_empty(), "{} claims to be implemented", feature.name);
            }
        }
    }

    #[test]
    fn a_library_builtin_names_the_function_it_is_and_the_type_to_call_it_with() {
        let abort = lookup(Kind::Builtin, "__builtin_abort").expect("in the table");
        assert_eq!(abort.library, "abort");
        assert_eq!(abort.signature, "void(void)");
        for feature in FEATURES {
            if feature.library.is_empty() {
                continue;
            }
            assert_eq!(feature.kind, Kind::Builtin, "{} is not a builtin", feature.name);
            assert!(!feature.signature.is_empty(), "{} has no type to call with", feature.name);
        }
    }

    /// Nearly every one of them is the name with the prefix taken off, which is the rule GCC
    /// documents. The field is written out anyway, so this is what checks the two agree. The
    /// one exception is `__builtin_cpu_init`, which GCC turns into a call to the libgcc function
    /// that fills in the CPU model. No row may name a function only rucc's own builtins archive
    /// defines, since an object calling one would not link anywhere else (#2191).
    #[test]
    fn the_library_function_is_the_name_without_the_prefix() {
        const OTHER_NAMES: &[(&str, &str)] = &[("__builtin_cpu_init", "__cpu_indicator_init")];
        for feature in FEATURES {
            if feature.library.is_empty() {
                continue;
            }
            let expected = match OTHER_NAMES.iter().find(|(name, _)| *name == feature.name) {
                Some((_, library)) => Some(*library),
                None => feature.name.strip_prefix("__builtin_"),
            };
            assert_eq!(expected, Some(feature.library), "{} names something else", feature.name);
            assert!(
                !feature.library.starts_with("__rucc_"),
                "{} needs rucc's archive",
                feature.name
            );
        }
    }

    #[test]
    fn a_c_attribute_answers_with_the_number_the_standard_gives_it() {
        let deprecated = lookup(Kind::CAttribute, "deprecated").expect("C23 has it");
        assert_eq!(deprecated.value, 202311);
        // The number C23 gave every one of them in the end, which is what gcc 16 answers.
        assert_eq!(has_c_attribute("nodiscard", LINUX), 202311);
        assert_eq!(has_attribute("fallthrough", LINUX), 202311);
        // And it is a different row from the GNU attribute of the same name.
        let gnu = lookup(Kind::Attribute, "deprecated").expect("GCC has it too");
        assert_eq!(gnu.value, 1);
    }

    /// The 32-bit conventions and `ms_struct` are there on every x86 target and the DLL
    /// attributes only on Windows, which is what gcc 16 answers on x86-64 Linux and with
    /// `-m32`, and what mingw-w64 gcc and AArch64 gcc answer.
    #[test]
    fn some_answers_are_the_targets() {
        let i686 = Target::new("i686", "linux");
        let windows = Target::new("x86_64", "windows");
        let arm = Target::new("aarch64", "linux");
        assert_eq!(LINUX, Target::new("x86_64", "linux"));
        // `cdecl` is what 32-bit x86 does anyway, so it is there too.
        assert_eq!(has_attribute("cdecl", i686), 1);
        assert_eq!(has_attribute("regparm", i686), 1);
        for name in ["cdecl", "stdcall", "fastcall", "thiscall", "regparm"] {
            assert_eq!(has_attribute(name, LINUX), 1, "{name}");
            assert_eq!(has_attribute(name, windows), 1, "{name}");
            assert_eq!(has_attribute(name, arm), 0, "{name}");
            // gcc has them on 32-bit x86 too, where they mean something this compiler does
            // not do yet, so the row says x86-64 and the answer there is no. `regparm` is done.
            if name != "cdecl" && name != "regparm" {
                assert_eq!(has_attribute(name, i686), 0, "{name}");
            }
        }
        for name in ["ms_struct", "gcc_struct", "ms_abi", "sysv_abi"] {
            for target in [LINUX, windows, i686] {
                assert_eq!(has_attribute(name, target), 1, "{name} on {target:?}");
            }
            assert_eq!(has_attribute(name, arm), 0, "{name}");
        }
        for name in ["dllimport", "dllexport", "selectany"] {
            assert_eq!(has_attribute(name, LINUX), 0, "{name}");
            assert_eq!(has_attribute(name, windows), 1, "{name}");
            assert_eq!(has_attribute(name, Target::new("aarch64", "windows")), 1, "{name}");
        }
        for target in [LINUX, windows, arm] {
            assert_eq!(has_attribute("vectorcall", target), 0, "gcc does not know it");
        }
        assert_eq!(has_builtin("__builtin_sponentry", arm), 1);
        assert_eq!(has_builtin("__builtin_sponentry", LINUX), 0);
    }

    #[test]
    fn ignoring_an_attribute_silently_is_a_decision_the_table_records() {
        let packed = lookup(Kind::Attribute, "packed").expect("in the table");
        assert_eq!(packed.answer, Answer::Error, "ignoring it would produce wrong code");
        let flatten = lookup(Kind::Attribute, "flatten").expect("in the table");
        assert_eq!(flatten.answer, Answer::Warn, "ignoring it would only produce slow code");
    }

    #[test]
    fn nested_functions_are_implemented_and_answer_what_gcc_answers() {
        let nested = lookup(Kind::Extension, "nested_functions").expect("in the table");
        assert_eq!(nested.status, Status::Implemented);
        assert_eq!(
            has_extension("nested_functions", Target::new("x86_64", "linux")),
            0,
            "gcc 16 answers 0 for the name"
        );
    }

    #[test]
    fn milestone_is_recorded() {
        assert!(MILESTONE.starts_with('M'));
    }
}
