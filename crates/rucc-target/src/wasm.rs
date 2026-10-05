//! The WebAssembly features a unit is built for.
//!
//! Design: #2863, and the WebAssembly plan, document 11.3 and decision D4.
//!
//! WebAssembly grows by proposals, and an engine and a library each support some of them. A unit
//! says which ones it may use. clang names a set with `-mcpu=` and changes one feature with
//! `-m<feature>` or `-mno-<feature>`, and rucc takes the same names. Each feature that has a
//! macro defines `__wasm_<feature>__`, with `_` in place of `-`, and the headers of wasi-libc and
//! of `wasm_simd128.h` read these macros.
//!
//! The default is `lime1`, because wasi-sdk 34 builds wasi-libc with it. So a rucc object asks
//! for no feature that the library does not already ask for. clang's own default is `generic`,
//! which adds `bulk-memory` and `reference-types`.
//!
//! The sets and the implications below are clang 23's, measured with `-dM` from wasi-sdk 34.
//! A feature that needs another one turns the other one on too: `bulk-memory` turns on
//! `bulk-memory-opt`, and `relaxed-simd` turns on `simd128`.

use std::fmt;

use crate::Preview;

/// One WebAssembly feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Feature {
    /// Sign extension operators.
    SignExt,
    /// Float to integer conversions that saturate and do not trap.
    NontrappingFptoint,
    /// `memory.copy` and `memory.fill`, with no passive segments.
    BulkMemoryOpt,
    /// The full bulk memory proposal, with passive segments.
    BulkMemory,
    /// Blocks and functions with more than one result.
    Multivalue,
    /// Globals that can be written, which the stack pointer is.
    MutableGlobals,
    /// Constant expressions with `add`, `sub` and `mul`.
    ExtendedConst,
    /// The long form of the table index of `call_indirect`. It has no macro.
    CallIndirectOverlong,
    /// `externref`, `funcref` and more than one table.
    ReferenceTypes,
    /// Exception handling with `exnref` and `try_table`.
    ExceptionHandling,
    /// `return_call` and `return_call_indirect`.
    TailCall,
    /// 128-bit SIMD.
    Simd128,
    /// SIMD operators whose result can depend on the engine.
    RelaxedSimd,
    /// 128-bit add, subtract and multiply.
    WideArithmetic,
    /// Shared memory and atomic operators.
    Atomics,
    /// More than one memory.
    Multimemory,
    /// Garbage collected types.
    Gc,
    /// Half precision lanes in SIMD.
    Fp16,
    /// A short encoding of imports. It has no macro.
    CompactImports,
    /// Atomic operators with orders weaker than sequential consistency.
    RelaxedAtomics,
}

impl Feature {
    /// Every feature, in the order of the table in document 11.3.
    pub const ALL: [Feature; 20] = [
        Feature::SignExt,
        Feature::NontrappingFptoint,
        Feature::BulkMemoryOpt,
        Feature::BulkMemory,
        Feature::Multivalue,
        Feature::MutableGlobals,
        Feature::ExtendedConst,
        Feature::CallIndirectOverlong,
        Feature::ReferenceTypes,
        Feature::ExceptionHandling,
        Feature::TailCall,
        Feature::Simd128,
        Feature::RelaxedSimd,
        Feature::WideArithmetic,
        Feature::Atomics,
        Feature::Multimemory,
        Feature::Gc,
        Feature::Fp16,
        Feature::CompactImports,
        Feature::RelaxedAtomics,
    ];

    /// The name, as `-m<name>` and the `target_features` section spell it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Feature::SignExt => "sign-ext",
            Feature::NontrappingFptoint => "nontrapping-fptoint",
            Feature::BulkMemoryOpt => "bulk-memory-opt",
            Feature::BulkMemory => "bulk-memory",
            Feature::Multivalue => "multivalue",
            Feature::MutableGlobals => "mutable-globals",
            Feature::ExtendedConst => "extended-const",
            Feature::CallIndirectOverlong => "call-indirect-overlong",
            Feature::ReferenceTypes => "reference-types",
            Feature::ExceptionHandling => "exception-handling",
            Feature::TailCall => "tail-call",
            Feature::Simd128 => "simd128",
            Feature::RelaxedSimd => "relaxed-simd",
            Feature::WideArithmetic => "wide-arithmetic",
            Feature::Atomics => "atomics",
            Feature::Multimemory => "multimemory",
            Feature::Gc => "gc",
            Feature::Fp16 => "fp16",
            Feature::CompactImports => "compact-imports",
            Feature::RelaxedAtomics => "relaxed-atomics",
        }
    }

    /// The feature with this name, if there is one.
    #[must_use]
    pub fn named(name: &str) -> Option<Feature> {
        Feature::ALL.into_iter().find(|f| f.name() == name)
    }

    /// The macro that says the feature is on, or nothing for the two features that have none.
    #[must_use]
    pub fn macro_name(self) -> Option<String> {
        match self {
            Feature::CallIndirectOverlong | Feature::CompactImports => None,
            _ => Some(format!("__wasm_{}__", self.name().replace('-', "_"))),
        }
    }

    /// The features that this one turns on with it.
    #[must_use]
    pub const fn needs(self) -> &'static [Feature] {
        match self {
            Feature::BulkMemory => &[Feature::BulkMemoryOpt],
            Feature::ReferenceTypes => &[Feature::CallIndirectOverlong],
            Feature::RelaxedSimd | Feature::Fp16 => &[Feature::Simd128],
            Feature::Gc => &[Feature::ReferenceTypes],
            _ => &[],
        }
    }

    const fn bit(self) -> u32 {
        1 << self as u32
    }
}

impl fmt::Display for Feature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A set of features.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Features(u32);

impl Features {
    /// No feature, which is the `mvp` set.
    pub const NONE: Features = Features(0);

    /// The set with these features and the features that they need.
    #[must_use]
    pub fn of(features: &[Feature]) -> Features {
        features.iter().fold(Features::NONE, |set, &f| set.with(f))
    }

    /// Whether the feature is in the set.
    #[must_use]
    pub const fn has(self, feature: Feature) -> bool {
        self.0 & feature.bit() != 0
    }

    /// The set with the feature and the features that it needs.
    #[must_use]
    pub fn with(self, feature: Feature) -> Features {
        let set = Features(self.0 | feature.bit());
        feature.needs().iter().fold(set, |set, &f| set.with(f))
    }

    /// The features in either set.
    #[must_use]
    pub const fn union(self, other: Features) -> Features {
        Features(self.0 | other.0)
    }

    /// The features in the set, in the order of [`Feature::ALL`].
    pub fn iter(self) -> impl Iterator<Item = Feature> {
        Feature::ALL.into_iter().filter(move |&f| self.has(f))
    }

    /// The macros of the features in the set, in the order of [`Feature::ALL`].
    pub fn macros(self) -> impl Iterator<Item = String> {
        self.iter().filter_map(Feature::macro_name)
    }
}

/// The features a WASI preview turns on whatever the set is.
///
/// wasip3 has cooperative threads, so clang 23 builds for it as if `-pthread` was given. That
/// turns on `bulk-memory`, `mutable-globals` and `sign-ext` on top of every set, `mvp` too, and
/// defines `__wasm_libcall_thread_context__`, which says that the stack pointer and the TLS base
/// are reached through library calls. No flag turns these off. The other previews turn on
/// nothing.
#[must_use]
pub fn required(preview: Preview) -> Features {
    match preview {
        Preview::P1 | Preview::P2 => Features::NONE,
        Preview::P3 => {
            Features::of(&[Feature::BulkMemory, Feature::MutableGlobals, Feature::SignExt])
        }
    }
}

/// A set of features with a name, which `-mcpu=` selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Cpu {
    /// No feature after the first version of WebAssembly.
    Mvp,
    /// clang's default.
    Generic,
    /// The set that wasi-sdk 34 builds wasi-libc with, and rucc's default (decision D4).
    #[default]
    Lime1,
    /// Every feature that clang 23 implements.
    BleedingEdge,
}

impl Cpu {
    /// Every set, in the order that a message lists them.
    pub const ALL: [Cpu; 4] = [Cpu::Mvp, Cpu::Generic, Cpu::Lime1, Cpu::BleedingEdge];

    /// The name, as `-mcpu=` spells it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Cpu::Mvp => "mvp",
            Cpu::Generic => "generic",
            Cpu::Lime1 => "lime1",
            Cpu::BleedingEdge => "bleeding-edge",
        }
    }

    /// The set with this name, if there is one.
    #[must_use]
    pub fn named(name: &str) -> Option<Cpu> {
        Cpu::ALL.into_iter().find(|c| c.name() == name)
    }

    /// The features of the set.
    #[must_use]
    pub fn features(self) -> Features {
        use Feature::*;
        match self {
            Cpu::Mvp => Features::NONE,
            Cpu::Generic => Features::of(&[
                BulkMemory,
                BulkMemoryOpt,
                CallIndirectOverlong,
                Multivalue,
                MutableGlobals,
                NontrappingFptoint,
                ReferenceTypes,
                SignExt,
            ]),
            Cpu::Lime1 => Features::of(&[
                BulkMemoryOpt,
                CallIndirectOverlong,
                ExtendedConst,
                Multivalue,
                MutableGlobals,
                NontrappingFptoint,
                SignExt,
            ]),
            // Every feature but `compact-imports`, which clang 23 does not put in the set.
            Cpu::BleedingEdge => Features::of(
                &Feature::ALL.into_iter().filter(|&f| f != CompactImports).collect::<Vec<_>>(),
            ),
        }
    }
}

impl fmt::Display for Cpu {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn macros(set: Features) -> Vec<String> {
        let mut names: Vec<String> = set.macros().collect();
        names.sort();
        names
    }

    #[test]
    fn each_set_defines_the_macros_that_clang_23_defines() {
        // `clang --target=wasm32-unknown-unknown -mcpu=<name> -E -dM`, from wasi-sdk 34.
        assert!(macros(Cpu::Mvp.features()).is_empty());
        assert_eq!(
            macros(Cpu::Lime1.features()),
            [
                "__wasm_bulk_memory_opt__",
                "__wasm_extended_const__",
                "__wasm_multivalue__",
                "__wasm_mutable_globals__",
                "__wasm_nontrapping_fptoint__",
                "__wasm_sign_ext__",
            ]
        );
        assert_eq!(
            macros(Cpu::Generic.features()),
            [
                "__wasm_bulk_memory__",
                "__wasm_bulk_memory_opt__",
                "__wasm_multivalue__",
                "__wasm_mutable_globals__",
                "__wasm_nontrapping_fptoint__",
                "__wasm_reference_types__",
                "__wasm_sign_ext__",
            ]
        );
        assert_eq!(macros(Cpu::BleedingEdge.features()).len(), 18);
        assert_eq!(Cpu::default(), Cpu::Lime1);
    }

    #[test]
    fn a_feature_turns_on_what_it_needs() {
        // Each one alone on top of `mvp`, as clang 23 answers it.
        let alone = |f| macros(Features::NONE.with(f));
        assert_eq!(
            alone(Feature::BulkMemory),
            ["__wasm_bulk_memory__", "__wasm_bulk_memory_opt__"]
        );
        assert_eq!(alone(Feature::RelaxedSimd), ["__wasm_relaxed_simd__", "__wasm_simd128__"]);
        assert_eq!(alone(Feature::Fp16), ["__wasm_fp16__", "__wasm_simd128__"]);
        assert_eq!(alone(Feature::Gc), ["__wasm_gc__", "__wasm_reference_types__"]);
        assert!(alone(Feature::CallIndirectOverlong).is_empty());
        assert!(alone(Feature::CompactImports).is_empty());
        assert!(Features::NONE.with(Feature::Gc).has(Feature::CallIndirectOverlong));
    }

    #[test]
    fn wasip3_turns_on_what_clang_23_turns_on_there() {
        // `clang --target=wasm32-wasip3 -mcpu=mvp -E -dM`, less the thread context macro, which
        // is not a feature.
        assert_eq!(
            macros(required(Preview::P3)),
            [
                "__wasm_bulk_memory__",
                "__wasm_bulk_memory_opt__",
                "__wasm_mutable_globals__",
                "__wasm_sign_ext__",
            ]
        );
        assert_eq!(required(Preview::P1), Features::NONE);
        assert_eq!(required(Preview::P2), Features::NONE);
    }

    #[test]
    fn every_name_reads_back() {
        for f in Feature::ALL {
            assert_eq!(Feature::named(f.name()), Some(f));
        }
        for c in Cpu::ALL {
            assert_eq!(Cpu::named(c.name()), Some(c));
        }
        assert_eq!(Feature::named("simd"), None);
        assert_eq!(Cpu::named("lime2"), None);
    }
}
