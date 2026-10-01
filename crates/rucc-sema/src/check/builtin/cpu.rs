//! `__builtin_cpu_init`, `__builtin_cpu_supports` and `__builtin_cpu_is`, which ask the processor
//! the program is running on what it is.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! This is how GCC code decides at run time whether to call the version of a function built with
//! `__attribute__((target("sse4.2")))` or the plain one, and configure scripts probe for the pair
//! for that reason. None of the three asks the processor itself. libgcc does that once, in
//! `__cpu_indicator_init`, which runs as a constructor before `main` and writes what it found into
//! two objects of its own: `__cpu_model`, four `unsigned int`s holding the vendor, the type, the
//! subtype and the first thirty two feature bits, and `__cpu_features2`, three more words of
//! feature bits. The builtins are reads of those.
//!
//! # What each one becomes
//!
//! `__builtin_cpu_init()` is a call to `__cpu_indicator_init`, which is what the `library` on its
//! row says, so nothing here has to do anything with it on x86-64. A program calls it from a
//! constructor of its own that may run before libgcc's, and the function returns at once if it
//! has already run.
//!
//! `__builtin_cpu_supports("name")` is the word the feature's bit is in, with every other bit
//! cleared, which is [`ExprKind::CpuModel`] with a [`CpuTest::Bit`]. The answer is the bit where
//! it stands and not a one, because that is what gcc answers: `__builtin_cpu_supports("sse4.2")`
//! is 256 on a processor that has it, and a program that prints it prints 256 under either
//! compiler. The top bit of a word is the one exception, answered as one, since as an `int` the
//! bit itself would be negative.
//!
//! `__builtin_cpu_is("name")` is whether the vendor, type or subtype word is one number, which is
//! a [`CpuTest::Equals`] and answers one or zero.
//!
//! All three are gcc 16.2.0's lowering, measured and not read off the manual. The bit numbers are
//! gcc's `enum processor_features` and the vendor, type and subtype numbers are its
//! `processor_vendor`, `processor_types` and `processor_subtypes`, and every name in the two
//! tables below was compiled with gcc 16.2.0 at `-O2` and the word and the mask or number it read
//! compared with the row. They have to agree with libgcc's and not only with gcc's, since libgcc
//! is what writes the words, and they do because the two are built out of the same header.
//!
//! # Why the name has to be a string literal
//!
//! The name picks the word and the bit, and those are the instruction, so a name that is not
//! known until the program runs has nothing to pick with. gcc refuses it in the words used here,
//! and refuses a name it does not know the same way it does, which is an error and not a zero:
//! a program asking about a feature the compiler has never heard of is asking a question whose
//! answer it cannot use.
//!
//! # Only on x86-64
//!
//! The objects are libgcc's for x86 and nothing on another target defines them, so a call there
//! would link against a name no library has. It is refused where it is written instead.

use rucc_base::Symbol;
use rucc_diag::{Diagnostic, Span};
use rucc_types::TypeId;

use crate::check::Checker;
use crate::expr::{Category, CpuObject, CpuTest, Expr, ExprId, ExprKind};

/// The code everything this file refuses is reported under.
const CODE: &str = "E0726";

/// The three names, which are the whole of what this recognises.
const INIT: &str = "__builtin_cpu_init";
const SUPPORTS: &str = "__builtin_cpu_supports";
const IS: &str = "__builtin_cpu_is";

/// The word of `__cpu_model` a `__builtin_cpu_is` name is compared with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    /// The first word, `__cpu_vendor`.
    Vendor,
    /// The second, `__cpu_type`.
    Type,
    /// The third, `__cpu_subtype`.
    Subtype,
}

/// Every name `__builtin_cpu_supports` takes, with the feature's number in gcc's
/// `enum processor_features`.
///
/// The first thirty two are the bits of the fourth word of `__cpu_model` and the rest are the
/// bits of `__cpu_features2` from its first word on, which is where libgcc writes them. gcc 16's
/// `isa_names_table`, in its order, and all of it.
const FEATURES: &[(&str, u8)] = &[
    ("cmov", 0),
    ("mmx", 1),
    ("popcnt", 2),
    ("sse", 3),
    ("sse2", 4),
    ("sse3", 5),
    ("ssse3", 6),
    ("sse4.1", 7),
    ("sse4.2", 8),
    ("avx", 9),
    ("avx2", 10),
    ("sse4a", 11),
    ("fma4", 12),
    ("xop", 13),
    ("fma", 14),
    ("avx512f", 15),
    ("bmi", 16),
    ("bmi2", 17),
    ("aes", 18),
    ("pclmul", 19),
    ("avx512vl", 20),
    ("avx512bw", 21),
    ("avx512dq", 22),
    ("avx512cd", 23),
    ("avx512vbmi", 26),
    ("avx512ifma", 27),
    ("avx512vpopcntdq", 30),
    ("avx512vbmi2", 31),
    ("gfni", 32),
    ("vpclmulqdq", 33),
    ("avx512vnni", 34),
    ("avx512bitalg", 35),
    ("avx512bf16", 36),
    ("avx512vp2intersect", 37),
    ("3dnow", 38),
    ("3dnowp", 39),
    ("adx", 40),
    ("abm", 41),
    ("cldemote", 42),
    ("clflushopt", 43),
    ("clwb", 44),
    ("clzero", 45),
    ("cmpxchg16b", 46),
    ("cmpxchg8b", 47),
    ("enqcmd", 48),
    ("f16c", 49),
    ("fsgsbase", 50),
    ("fxsave", 51),
    ("hle", 52),
    ("ibt", 53),
    ("lahf_lm", 54),
    ("lm", 55),
    ("lwp", 56),
    ("lzcnt", 57),
    ("movbe", 58),
    ("movdir64b", 59),
    ("movdiri", 60),
    ("mwaitx", 61),
    ("osxsave", 62),
    ("pconfig", 63),
    ("pku", 64),
    ("prfchw", 66),
    ("ptwrite", 67),
    ("rdpid", 68),
    ("rdrnd", 69),
    ("rdseed", 70),
    ("rtm", 71),
    ("serialize", 72),
    ("sgx", 73),
    ("sha", 74),
    ("shstk", 75),
    ("tbm", 76),
    ("tsxldtrk", 77),
    ("vaes", 78),
    ("waitpkg", 79),
    ("wbnoinvd", 80),
    ("xsave", 81),
    ("xsavec", 82),
    ("xsaveopt", 83),
    ("xsaves", 84),
    ("amx-tile", 85),
    ("amx-int8", 86),
    ("amx-bf16", 87),
    ("uintr", 88),
    ("hreset", 89),
    ("kl", 90),
    ("aeskle", 91),
    ("widekl", 92),
    ("avxvnni", 93),
    ("avx512fp16", 94),
    ("x86-64", 95),
    ("x86-64-v2", 96),
    ("x86-64-v3", 97),
    ("x86-64-v4", 98),
    ("avxifma", 99),
    ("avxvnniint8", 100),
    ("avxneconvert", 101),
    ("cmpccxadd", 102),
    ("amx-fp16", 103),
    ("prefetchi", 104),
    ("raoint", 105),
    ("amx-complex", 106),
    ("avxvnniint16", 107),
    ("sm3", 108),
    ("sha512", 109),
    ("sm4", 110),
    ("apxf", 111),
    ("usermsr", 112),
    ("avx10.1", 114),
    ("avx10.2", 116),
    ("amx-avx512", 117),
    ("amx-tf32", 118),
    ("amx-fp8", 120),
    ("movrs", 121),
    ("amx-movrs", 122),
    ("avx512bmm", 123),
];

/// Every name `__builtin_cpu_is` takes, with the word of `__cpu_model` it is compared with and the
/// number it is compared against.
///
/// gcc 16's `processor_alias_table`, the rows of it that carry a model, in its order. Several
/// names share a number because gcc's table says they do: `raptorlake` and `meteorlake` are both
/// libgcc's Alder Lake.
const MODELS: &[(&str, Field, u32)] = &[
    ("core2", Field::Type, 2),
    ("nehalem", Field::Subtype, 1),
    ("corei7", Field::Type, 3),
    ("westmere", Field::Subtype, 2),
    ("sandybridge", Field::Subtype, 3),
    ("ivybridge", Field::Subtype, 12),
    ("haswell", Field::Subtype, 13),
    ("broadwell", Field::Subtype, 14),
    ("skylake", Field::Subtype, 15),
    ("skylake-avx512", Field::Subtype, 16),
    ("cannonlake", Field::Subtype, 17),
    ("icelake-client", Field::Subtype, 18),
    ("rocketlake", Field::Subtype, 27),
    ("icelake-server", Field::Subtype, 19),
    ("cascadelake", Field::Subtype, 21),
    ("tigerlake", Field::Subtype, 22),
    ("cooperlake", Field::Subtype, 23),
    ("sapphirerapids", Field::Subtype, 24),
    ("emeraldrapids", Field::Subtype, 24),
    ("alderlake", Field::Subtype, 25),
    ("raptorlake", Field::Subtype, 25),
    ("meteorlake", Field::Subtype, 25),
    ("graniterapids", Field::Subtype, 30),
    ("graniterapids-d", Field::Subtype, 31),
    ("arrowlake", Field::Subtype, 32),
    ("arrowlake-s", Field::Subtype, 33),
    ("lunarlake", Field::Subtype, 33),
    ("pantherlake", Field::Subtype, 34),
    ("diamondrapids", Field::Subtype, 38),
    ("wildcatlake", Field::Subtype, 34),
    ("novalake", Field::Subtype, 39),
    ("bonnell", Field::Type, 1),
    ("atom", Field::Type, 1),
    ("silvermont", Field::Type, 6),
    ("slm", Field::Type, 6),
    ("goldmont", Field::Type, 12),
    ("goldmont-plus", Field::Type, 13),
    ("tremont", Field::Type, 14),
    ("gracemont", Field::Subtype, 25),
    ("sierraforest", Field::Type, 17),
    ("grandridge", Field::Type, 18),
    ("clearwaterforest", Field::Type, 19),
    ("intel", Field::Vendor, 1),
    ("lujiazui", Field::Subtype, 28),
    ("yongfeng", Field::Subtype, 35),
    ("shijidadao", Field::Subtype, 37),
    ("barcelona", Field::Subtype, 4),
    ("bdver1", Field::Subtype, 7),
    ("bdver2", Field::Subtype, 8),
    ("bdver3", Field::Subtype, 9),
    ("bdver4", Field::Subtype, 10),
    ("znver1", Field::Subtype, 11),
    ("znver2", Field::Subtype, 20),
    ("znver3", Field::Subtype, 26),
    ("znver4", Field::Subtype, 29),
    ("znver5", Field::Subtype, 36),
    ("znver6", Field::Subtype, 40),
    ("btver1", Field::Type, 8),
    ("btver2", Field::Type, 9),
    ("c86-4g-m4", Field::Subtype, 41),
    ("c86-4g-m6", Field::Subtype, 42),
    ("c86-4g-m7", Field::Subtype, 43),
    ("c86-4g-m8", Field::Subtype, 44),
    ("amd", Field::Vendor, 2),
    ("amdfam10h", Field::Type, 4),
    ("amdfam15h", Field::Type, 5),
    ("amdfam17h", Field::Type, 10),
    ("amdfam19h", Field::Type, 15),
    ("shanghai", Field::Subtype, 5),
    ("istanbul", Field::Subtype, 6),
    ("hygon", Field::Vendor, 4),
    ("hygonfam18h", Field::Type, 21),
];

/// Which of the three a name is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ask {
    Init,
    Supports,
    Is,
}

impl Checker<'_> {
    /// What a call to one of the three becomes, if the name is one of them.
    ///
    /// Answers nothing for every other call in the program, and nothing for `__builtin_cpu_init`
    /// on x86-64 either, since the call already made is the answer there. Taken after the call has
    /// been checked, for the reason `check/builtin/trap.rs` gives: the rows carry signatures, so
    /// the prototype is what reports a call with the wrong number of arguments, and the type the
    /// answer has is the one the row gives it.
    pub(in crate::check) fn cpu_builtin(
        &mut self,
        function: Option<Symbol>,
        args: &[ExprId],
        ret: TypeId,
        span: Span,
    ) -> Option<ExprId> {
        let name = function?;
        let spelled = self.text(name);
        if !spelled.starts_with("__builtin_cpu_") {
            return None;
        }
        let ask = match spelled {
            INIT => Ask::Init,
            SUPPORTS => Ask::Supports,
            IS => Ask::Is,
            _ => return None,
        };
        let spelled = spelled.to_owned();
        if self.cx.target.tuple.arch().as_str() != "x86_64" {
            let what = format!("`{spelled}` is only available on x86-64");
            let note = "what it reads is written by the x86 half of libgcc, and nothing on this \
                        target defines it";
            self.report(Diagnostic::error(what, span).with_code(CODE).note(note, span));
            return Some(self.poison(span));
        }
        if ask == Ask::Init {
            return None;
        }
        // No argument at all, which the prototype has already refused.
        let &arg = args.first()?;
        if self.is_poisoned(arg) {
            return Some(self.poison(span));
        }
        let Some(text) = self.literal_text(arg) else {
            let at = self.tast.expr_span(arg);
            let what = "parameter to builtin must be a string constant or literal";
            let note = "the name picks the word and the bit that are read, so it has to be known \
                        when the program is compiled";
            self.report(Diagnostic::error(what, at).with_code(CODE).note(note, at));
            return Some(self.poison(span));
        };
        let node = match ask {
            Ask::Supports => FEATURES.iter().find(|row| row.0 == text).map(|&(_, feature)| {
                let (object, word, bit) = feature_word(feature);
                ExprKind::CpuModel { object, word, test: CpuTest::Bit(bit) }
            }),
            _ => MODELS.iter().find(|row| row.0 == text).map(|&(_, field, value)| {
                let word = match field {
                    Field::Vendor => 0,
                    Field::Type => 1,
                    Field::Subtype => 2,
                };
                ExprKind::CpuModel { object: CpuObject::Model, word, test: CpuTest::Equals(value) }
            }),
        };
        let Some(node) = node else {
            let at = self.tast.expr_span(arg);
            let what = format!("parameter to builtin not valid: {text}");
            self.report(Diagnostic::error(what, at).with_code(CODE));
            return Some(self.poison(span));
        };
        Some(self.tast.expr(Expr::new(node, ret, Category::Rvalue), span))
    }

    /// The characters of a string literal the argument is, under whatever conversions checking
    /// the call put around it, and nothing for anything else.
    ///
    /// The conversions are there because the parameter is a `const char *` and the literal is an
    /// array, and a cast the program wrote is looked through too, which is what gcc does.
    fn literal_text(&self, mut arg: ExprId) -> Option<String> {
        loop {
            match self.tast[arg].kind {
                ExprKind::Convert { operand, .. } | ExprKind::Cast(operand) => arg = operand,
                ExprKind::Str(id) => {
                    let elements = &self.tast[id].elements;
                    return Some(
                        elements.iter().filter_map(|&unit| char::from_u32(unit)).collect(),
                    );
                }
                _ => return None,
            }
        }
    }
}

/// Where libgcc keeps a feature's bit: the object, the word of it and the bit in the word.
///
/// The first thirty two features are the last word of `__cpu_model`, and every one after that is
/// in `__cpu_features2`, thirty two to a word, starting again from its first.
pub(in crate::check) fn feature_word(feature: u8) -> (CpuObject, u8, u8) {
    if feature < 32 {
        (CpuObject::Model, 3, feature)
    } else {
        let past = feature - 32;
        (CpuObject::Features2, past / 32, past % 32)
    }
}

#[cfg(test)]
mod tests {
    use rucc_gnu::{Kind, Status};

    use super::*;

    /// The three names are rows of the table with signatures, since the call is checked against
    /// them before this looks at it, and the first of them is a call to libgcc's function.
    #[test]
    fn the_names_are_rows_of_the_table_that_carry_a_signature() {
        for (name, signature, library) in [
            (INIT, "void(void)", "__cpu_indicator_init"),
            (SUPPORTS, "int(const char *)", ""),
            (IS, "int(const char *)", ""),
        ] {
            let Some(feature) = rucc_gnu::lookup(Kind::Builtin, name) else {
                panic!("{name} is answered here and is not in features.toml");
            };
            assert_eq!(feature.status, Status::Implemented, "{name}");
            assert_eq!(feature.signature, signature, "{name}");
            assert_eq!(feature.library, library, "{name}");
        }
    }

    /// Every feature has somewhere to be. `__cpu_features2` is three words, so the last feature
    /// number there is room for is a hundred and twenty seven, and gcc 16's last is below that.
    #[test]
    fn every_feature_is_a_bit_of_a_word_libgcc_has() {
        for &(name, feature) in FEATURES {
            let (object, word, bit) = feature_word(feature);
            assert!(u64::from(word) * 4 < object.size(), "{name}");
            assert!(bit < 32, "{name}");
        }
        assert_eq!(feature_word(8), (CpuObject::Model, 3, 8), "sse4.2");
        assert_eq!(feature_word(30), (CpuObject::Model, 3, 30), "avx512vpopcntdq");
        assert_eq!(feature_word(33), (CpuObject::Features2, 0, 1), "vpclmulqdq");
        assert_eq!(feature_word(81), (CpuObject::Features2, 1, 17), "xsave");
    }

    /// A name is in each table once, since a second row for it would be one nothing reads.
    #[test]
    fn no_name_is_in_a_table_twice() {
        let mut names: Vec<_> = FEATURES.iter().map(|row| row.0).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), FEATURES.len());
        let mut names: Vec<_> = MODELS.iter().map(|row| row.0).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), MODELS.len());
    }
}
