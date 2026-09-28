//! Which extensions of the x86-64 instruction set a unit is built for.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.5, for the macros this decides.
//!
//! x86-64 is not one instruction set. The psABI names a baseline, which is SSE2 and nothing above
//! it, and every processor sold since has more: SSE3 and its successors, the population count, the
//! CRC-32C step, AVX in its several generations. A program asks for more than the baseline in two
//! places. The command line says it for the whole unit, with `-msse4.2` or `-march=x86-64-v2`, and
//! a function says it for itself with `__attribute__((target("sse4.2")))`, which is how a program
//! that picks its fastest path at run time compiles the fast path without the rest of the unit
//! assuming it. Both are the same question in two places, so both are answered here, and the
//! answer is an [`Isa`]: a set of extensions, each one a bit.
//!
//! # Names and what they bring with them
//!
//! The names are gcc's, because the flags and the attribute strings are gcc's and a build that
//! works there has to work here. Every one gcc 16.2.0 takes is in [`FEATURES`], including the many
//! this compiler has no intrinsics for, because the attribute refuses a name it does not know and
//! a name gcc knows is not one this compiler may refuse. Knowing a name is not the same as
//! providing what it stands for, and [`Feature::honoured`] is the line between the two: only the
//! honoured ones can be turned on from the command line and only they get a macro, since a macro is
//! a promise that the intrinsics behind it exist and for AVX they do not yet.
//!
//! Turning one extension on turns on the ones it is built over, so `sse4.2` brings `sse4.1` and
//! everything under it. Turning one off turns off everything built over it, so `-mno-sse3` takes
//! SSSE3 and both SSE4 levels with it. Both directions are gcc's, and are the transitive closure of
//! the `needs` column in [`FEATURES`].
//!
//! # What was said and what was assumed
//!
//! gcc keeps two sets while it reads a command line: which extensions are on, and which ones the
//! command line said anything about, either way. The second is what lets `-mno-sse3
//! -march=x86-64-v2` mean no SSE3 whichever order the two come in, because a processor named by
//! `-march` supplies only the extensions nothing else spoke for. [`Choices`] is that pair.
//!
//! The population count and the CRC-32C step are the two that do not fit the tree. gcc turns both
//! on whenever SSE4.2 ends up on and neither was mentioned, and the test is made last, against the
//! finished set, so `-msse4.2 -mno-sse4.2` leaves neither behind while `-mpopcnt -msse4.2
//! -mno-sse4.2` keeps the population count it was asked for. These are all checked against gcc 16's
//! `-dM` output in the tests below.

use std::fmt;

/// One row of [`FEATURES`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    /// The name, as `-m` and the attribute string spell it.
    pub name: &'static str,
    /// The extensions this one is built over, by name, which turning this one on turns on too.
    pub needs: &'static [&'static str],
    /// Whether this compiler provides what the name stands for. See [`Feature::honoured`].
    pub honoured: bool,
}

/// A row this compiler provides.
const fn yes(name: &'static str, needs: &'static [&'static str]) -> Row {
    Row { name, needs, honoured: true }
}

/// A row this compiler knows the name of and nothing more.
const fn named(name: &'static str, needs: &'static [&'static str]) -> Row {
    Row { name, needs, honoured: false }
}

/// Every extension gcc 16.2.0 has a `-m` flag and an attribute name for.
///
/// The honoured ones first, in the order of the SSE line, and then the rest roughly in the order
/// gcc's own table has them. What each needs is gcc's `_SET` mask for it, cut down to the
/// extensions directly beneath it, since the closure is taken when the table is read.
pub static FEATURES: &[Row] = &[
    yes("mmx", &[]),
    yes("sse", &[]),
    yes("sse2", &["sse"]),
    yes("sse3", &["sse2"]),
    yes("ssse3", &["sse3"]),
    yes("sse4.1", &["ssse3"]),
    yes("sse4.2", &["sse4.1"]),
    yes("popcnt", &[]),
    yes("crc32", &[]),
    yes("fxsr", &[]),
    named("sse4a", &["sse3"]),
    named("3dnow", &["mmx"]),
    named("3dnowa", &["3dnow"]),
    yes("xsave", &[]),
    named("xsaveopt", &["xsave"]),
    named("xsavec", &["xsave"]),
    named("xsaves", &["xsave"]),
    named("avx", &["sse4.2", "xsave"]),
    named("avx2", &["avx"]),
    named("fma", &["avx"]),
    named("f16c", &["avx"]),
    named("fma4", &["sse4a", "avx"]),
    named("xop", &["fma4"]),
    named("avx512f", &["avx2", "fma", "f16c"]),
    named("avx512cd", &["avx512f"]),
    named("avx512dq", &["avx512f"]),
    named("avx512bw", &["avx512f"]),
    named("avx512vl", &["avx512f"]),
    named("avx512ifma", &["avx512f"]),
    named("avx512vbmi", &["avx512bw"]),
    named("avx512vbmi2", &["avx512bw"]),
    named("avx512vnni", &["avx512f"]),
    named("avx512bitalg", &["avx512bw"]),
    named("avx512vpopcntdq", &["avx512f"]),
    named("avx512bf16", &["avx512bw"]),
    named("avx512fp16", &["avx512bw"]),
    named("avx512vp2intersect", &["avx512f"]),
    named("avx512bmm", &["avx512bw"]),
    named("avx10.1", &["avx512vl", "avx512dq", "avx512cd", "avx512bf16", "avx512fp16"]),
    named("avx10.2", &["avx10.1"]),
    named("avxvnni", &["avx2"]),
    named("avxifma", &["avx2"]),
    named("avxneconvert", &["avx2"]),
    named("avxvnniint8", &["avx2"]),
    named("avxvnniint16", &["avx2"]),
    named("aes", &["sse2"]),
    named("pclmul", &["sse2"]),
    named("sha", &["sse2"]),
    named("gfni", &["sse2"]),
    named("vaes", &["avx", "aes"]),
    named("vpclmulqdq", &["avx", "pclmul"]),
    named("sha512", &["avx2"]),
    named("sm3", &["avx"]),
    named("sm4", &["avx2"]),
    named("abm", &[]),
    named("lzcnt", &[]),
    named("bmi", &[]),
    named("bmi2", &[]),
    named("tbm", &[]),
    named("movbe", &[]),
    named("cx16", &[]),
    named("sahf", &[]),
    named("rdrnd", &[]),
    named("rdseed", &[]),
    named("adx", &[]),
    named("prfchw", &[]),
    named("clflushopt", &[]),
    named("clwb", &[]),
    named("fsgsbase", &[]),
    named("rtm", &[]),
    named("hle", &[]),
    named("rdpid", &[]),
    named("lwp", &[]),
    named("pku", &[]),
    named("mwaitx", &[]),
    named("clzero", &[]),
    named("cldemote", &[]),
    named("movdiri", &[]),
    named("movdir64b", &[]),
    named("waitpkg", &[]),
    named("enqcmd", &[]),
    named("serialize", &[]),
    named("tsxldtrk", &[]),
    named("uintr", &[]),
    named("hreset", &[]),
    named("kl", &[]),
    named("widekl", &["kl"]),
    named("ptwrite", &[]),
    named("sgx", &[]),
    named("shstk", &[]),
    named("wbnoinvd", &[]),
    named("pconfig", &[]),
    named("cmpccxadd", &[]),
    named("raoint", &[]),
    named("prefetchi", &[]),
    named("usermsr", &[]),
    named("mwait", &[]),
    named("movrs", &[]),
    named("apxf", &[]),
    named("amx-tile", &[]),
    named("amx-int8", &["amx-tile"]),
    named("amx-bf16", &["amx-tile"]),
    named("amx-fp16", &["amx-tile"]),
    named("amx-complex", &["amx-tile"]),
    named("amx-avx512", &["amx-tile", "avx10.2"]),
    named("amx-fp8", &["amx-tile"]),
    named("amx-movrs", &["amx-tile"]),
    named("amx-tf32", &["amx-tile"]),
];

/// One extension, as its row in [`FEATURES`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Feature(u8);

impl Feature {
    /// The extension with that name, as `-m` and the attribute string spell it.
    ///
    /// `sse4` is gcc's name for turning on both SSE4 levels, which is SSE4.2 with what it needs, so
    /// it reads as `sse4.2`. It is a different name the other way round, where `-mno-sse4` turns
    /// off both levels and so means `sse4.1`, and [`Feature::named_off`] is that direction.
    #[must_use]
    pub fn named(name: &str) -> Option<Feature> {
        let name = if name == "sse4" { "sse4.2" } else { name };
        FEATURES.iter().position(|row| row.name == name).map(|at| Feature(at as u8))
    }

    /// The extension a `no-` in front of that name turns off.
    #[must_use]
    pub fn named_off(name: &str) -> Option<Feature> {
        Feature::named(if name == "sse4" { "sse4.1" } else { name })
    }

    /// Its row.
    #[must_use]
    pub fn row(self) -> &'static Row {
        &FEATURES[usize::from(self.0)]
    }

    /// Its name.
    #[must_use]
    pub fn name(self) -> &'static str {
        self.row().name
    }

    /// Whether this compiler provides what the name stands for.
    ///
    /// The SSE line up to 4.2, the population count, the CRC-32C step, and the three the baseline
    /// already has. For those the headers have the intrinsics and the assembler has the
    /// instructions. For every other name gcc knows, a program can say it and have it remembered,
    /// which is what an attribute on a function built for a processor chosen at run time needs, but
    /// the command line may not turn it on for a whole unit and no macro claims it.
    #[must_use]
    pub fn honoured(self) -> bool {
        self.row().honoured
    }

    /// The macro gcc defines while this extension is on: the name in capitals with the dots and
    /// dashes turned into underscores, so `sse4.2` is `__SSE4_2__`.
    #[must_use]
    pub fn macro_name(self) -> String {
        let spelled: String = self
            .name()
            .chars()
            .map(|c| if c == '.' || c == '-' { '_' } else { c.to_ascii_uppercase() })
            .collect();
        format!("__{spelled}__")
    }

    /// This extension alone.
    const fn bit(self) -> u128 {
        1 << self.0
    }
}

/// A set of extensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Isa(u128);

impl Isa {
    /// No extensions at all, which is what every target but x86-64 has here.
    pub const NONE: Isa = Isa(0);

    /// What every x86-64 processor has, which is what the psABI promises and what a unit is built
    /// for when nothing says otherwise: MMX, SSE, SSE2 and the instructions that save their state.
    #[must_use]
    pub fn baseline() -> Isa {
        Isa::of(&["mmx", "sse", "sse2", "fxsr"])
    }

    /// The extensions a processor gcc can be told the name of has, for the names that are a level
    /// of the psABI rather than a product: `x86-64`, which is the baseline, and the three levels
    /// above it that the psABI numbers. `None` for anything else, including a product name, which a
    /// caller takes to mean the baseline.
    ///
    /// Each level is its list in the psABI, section 3.1.1, which is also what gcc's
    /// `-march=x86-64-v2` defines macros for.
    #[must_use]
    pub fn level(name: &str) -> Option<Isa> {
        let v2 = [
            "mmx", "sse", "sse2", "fxsr", "sse3", "ssse3", "sse4.1", "sse4.2", "popcnt", "cx16",
            "sahf",
        ];
        let v3 = ["avx", "avx2", "bmi", "bmi2", "f16c", "fma", "lzcnt", "movbe", "xsave"];
        let v4 = ["avx512f", "avx512bw", "avx512cd", "avx512dq", "avx512vl"];
        let names: Vec<&str> = match name {
            "x86-64" => return Some(Isa::baseline()),
            "x86-64-v2" => v2.to_vec(),
            "x86-64-v3" => [&v2[..], &v3].concat(),
            "x86-64-v4" => [&v2[..], &v3, &v4].concat(),
            _ => return None,
        };
        Some(Isa::of(&names))
    }

    /// Those extensions and everything each of them is built over.
    ///
    /// # Panics
    ///
    /// On a name that is not in [`FEATURES`], which is a mistake in the caller.
    #[must_use]
    pub fn of(names: &[&str]) -> Isa {
        names.iter().fold(Isa::NONE, |isa, name| {
            let feature =
                Feature::named(name).unwrap_or_else(|| panic!("`{name}` is not a feature"));
            isa.union(Isa::with(feature))
        })
    }

    /// That extension and everything it is built over, which is what turning it on turns on.
    ///
    /// # Panics
    ///
    /// When a row of [`FEATURES`] names a prerequisite that is not a row itself, which the tests
    /// below would have caught.
    #[must_use]
    pub fn with(feature: Feature) -> Isa {
        let mut set = feature.bit();
        let mut grew = true;
        while grew {
            grew = false;
            for (at, row) in FEATURES.iter().enumerate() {
                if set & (1 << at) == 0 {
                    continue;
                }
                for need in row.needs {
                    let bit = Feature::named(need).expect("a need is a feature").bit();
                    if set & bit == 0 {
                        set |= bit;
                        grew = true;
                    }
                }
            }
        }
        Isa(set)
    }

    /// That extension and everything built over it, which is what turning it off turns off.
    #[must_use]
    pub fn without(feature: Feature) -> Isa {
        let set = (0..FEATURES.len())
            .filter(|&at| Isa::with(Feature(at as u8)).has(feature))
            .fold(0, |set, at| set | (1u128 << at));
        Isa(set)
    }

    /// Whether this set has that extension.
    #[must_use]
    pub const fn has(self, feature: Feature) -> bool {
        self.0 & feature.bit() != 0
    }

    /// Whether this set has every extension `other` has, which is the question a call from a
    /// function built for this set to a function built for `other` has to ask.
    #[must_use]
    pub const fn covers(self, other: Isa) -> bool {
        other.0 & !self.0 == 0
    }

    /// Both sets together.
    #[must_use]
    pub const fn union(self, other: Isa) -> Isa {
        Isa(self.0 | other.0)
    }

    /// This set without anything in `other`.
    #[must_use]
    pub const fn minus(self, other: Isa) -> Isa {
        Isa(self.0 & !other.0)
    }

    /// The extensions in the set, in the order of [`FEATURES`].
    pub fn features(self) -> impl Iterator<Item = Feature> {
        (0..FEATURES.len() as u8).map(Feature).filter(move |feature| self.has(*feature))
    }

    /// The macros this set defines, which are the honoured extensions' and no others.
    pub fn macros(self) -> impl Iterator<Item = String> {
        self.features().filter(|feature| feature.honoured()).map(Feature::macro_name)
    }
}

impl fmt::Display for Isa {
    /// The names, comma separated, which is how an attribute string would say the same set.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (at, feature) in self.features().enumerate() {
            if at > 0 {
                f.write_str(",")?;
            }
            f.write_str(feature.name())?;
        }
        Ok(())
    }
}

/// What a command line or an attribute string said about the extensions, in the order it said it.
///
/// See the module documentation for why this is two sets and not one. It is kept as the list of
/// what was said rather than as the two sets because of one more thing gcc does before it reads
/// the list at all: a flag cancels an earlier one with the same name and the other sign, so
/// `-msse4.2 -mno-sse4.2` is a command line that said nothing and leaves SSE3 on if the baseline
/// had it, where applying the two in turn would take SSE3 away with the level built over it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Choices {
    /// Each thing said: the name as written, the extension it names and whether it was on.
    said: Vec<(String, Feature, bool)>,
}

impl Choices {
    /// Nothing said yet.
    #[must_use]
    pub const fn new() -> Choices {
        Choices { said: Vec::new() }
    }

    /// Turns that extension on, and everything it is built over.
    pub fn enable(&mut self, feature: Feature) {
        self.say(feature.name(), feature, true);
    }

    /// Turns that extension off, and everything built over it.
    pub fn disable(&mut self, feature: Feature) {
        self.say(feature.name(), feature, false);
    }

    /// The flag or the attribute option `name`, with or without a `no-` in front of it, which is
    /// how both are written.
    ///
    /// # Errors
    ///
    /// The name, when it is not one gcc knows.
    pub fn read(&mut self, text: &str) -> Result<(), String> {
        let (name, on) = match text.strip_prefix("no-") {
            Some(name) => (name, false),
            None => (text, true),
        };
        let feature = if on { Feature::named(name) } else { Feature::named_off(name) };
        let feature = feature.ok_or_else(|| name.to_owned())?;
        self.say(name, feature, on);
        Ok(())
    }

    /// One more thing said, cancelling an earlier one with the same name and the other sign.
    fn say(&mut self, name: &str, feature: Feature, on: bool) {
        self.said.retain(|(before, _, was)| !(before == name && *was != on));
        self.said.push((name.to_owned(), feature, on));
    }

    /// Whether anything was said at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.said.is_empty()
    }

    /// Every extension named and whether it was turned on or off, in the order they were said,
    /// which is what a caller refusing to turn on an extension it does not honour looks at.
    pub fn mentioned(&self) -> impl Iterator<Item = (Feature, bool)> + '_ {
        self.said.iter().map(|(_, feature, on)| (*feature, *on))
    }

    /// What was said, applied over `base`, which is what the processor named by `-march` has or,
    /// for an attribute, what the rest of the unit was built for.
    ///
    /// `base` supplies whatever was not mentioned, and then the population count and the CRC-32C
    /// step follow SSE4.2 unless they were mentioned themselves. See the module documentation.
    ///
    /// # Panics
    ///
    /// Never in practice: the three names it looks up are rows of [`FEATURES`].
    #[must_use]
    pub fn over(&self, base: Isa) -> Isa {
        let (mut on, mut told) = (Isa::NONE, Isa::NONE);
        for &(_, feature, turned_on) in &self.said {
            if turned_on {
                let set = Isa::with(feature);
                on = on.union(set);
                told = told.union(set);
            } else {
                let set = Isa::without(feature);
                on = on.minus(set);
                told = told.union(set);
            }
        }
        let mut isa = base.minus(told).union(on);
        let sse42 = Feature::named("sse4.2").expect("a feature");
        if isa.has(sse42) {
            for name in ["popcnt", "crc32"] {
                let feature = Feature::named(name).expect("a feature");
                if !told.has(feature) {
                    isa = isa.union(Isa(feature.bit()));
                }
            }
        }
        isa
    }
}

/// What the `target` attributes on one function said, which is [`Choices`] and the processor an
/// `arch=` named, if one did.
///
/// A function can carry more than one attribute and each can have more than one string, and gcc
/// reads them all as one comma separated list, so this is filled one string at a time and read once
/// at the end, by [`Target::over`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Target {
    /// The extensions named, with and without `no-`.
    choices: Choices,
    /// What the last `arch=` said the processor has, when it named a level of the psABI.
    arch: Option<Isa>,
    /// Whether the list was the single word `default`, which is the version of a function built
    /// for the unit that the other versions stand beside.
    default: bool,
    /// How many options were read, which `default` needs to be alone among.
    options: usize,
}

/// Why a `target` attribute string was refused, which is one of gcc's three messages for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetRefusal {
    /// A name gcc does not know, or `default` alongside something else.
    Unknown(String),
    /// A known option with a value gcc does not know, like `fpmath=bogus`.
    Value(String),
    /// A `no-` in front of an option that has no negated form.
    Negated(String),
}

impl fmt::Display for TargetRefusal {
    /// gcc 16's wording, so a build log reads the same under either compiler.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TargetRefusal::Unknown(name) => {
                write!(f, "attribute 'target' argument '{name}' is unknown")
            }
            TargetRefusal::Value(option) => {
                write!(f, "attribute value '{option}' is unknown in 'target' attribute")
            }
            TargetRefusal::Negated(name) => {
                write!(f, "pragma or attribute 'target(\"{name}\")' does not allow a negated form")
            }
        }
    }
}

/// The options a `target` attribute may carry that say nothing about the instruction set, each as
/// its name and whether a `no-` may stand in front of it. They choose how code is tuned or how
/// floating point is done, which this compiler has one answer to, so each is accepted and has no
/// effect.
const PLAIN_OPTIONS: &[(&str, bool)] = &[
    ("80387", true),
    ("fancy-math-387", true),
    ("ieee-fp", true),
    ("inline-all-stringops", true),
    ("inline-stringops-dynamically", true),
    ("align-stringops", true),
    ("recip", true),
    ("cld", true),
    ("general-regs-only", false),
];

impl Target {
    /// Nothing read yet.
    #[must_use]
    pub fn new() -> Target {
        Target::default()
    }

    /// One string of a `target` attribute, which is a comma separated list of options.
    ///
    /// An empty string says nothing, and gcc only warns about it. Nothing is trimmed, because gcc
    /// trims nothing either, so `" sse4.2"` is a name it does not know.
    ///
    /// # Errors
    ///
    /// The first option gcc would refuse, and why.
    pub fn read(&mut self, text: &str) -> Result<(), TargetRefusal> {
        if text.is_empty() {
            return Ok(());
        }
        for option in text.split(',') {
            self.option(option)?;
        }
        Ok(())
    }

    /// One option from the list.
    fn option(&mut self, option: &str) -> Result<(), TargetRefusal> {
        self.options += 1;
        if option == "default" {
            self.default = true;
        }
        if self.default && self.options > 1 {
            return Err(TargetRefusal::Unknown("default".to_owned()));
        }
        if option == "default" {
            return Ok(());
        }
        let (name, negated) = match option.strip_prefix("no-") {
            Some(name) => (name, true),
            None => (option, false),
        };
        if let Some((key, value)) = name.split_once('=') {
            return self.valued(option, key, value, negated);
        }
        if let Some(&(_, negatable)) = PLAIN_OPTIONS.iter().find(|(known, _)| *known == name) {
            return if negated && !negatable {
                Err(TargetRefusal::Negated(name.to_owned()))
            } else {
                Ok(())
            };
        }
        self.choices.read(option).map_err(|_| TargetRefusal::Unknown(option.to_owned()))
    }

    /// An option with a value: the processor to build for or to tune for, and two choices about
    /// floating point and vector width that change nothing here.
    ///
    /// gcc refuses a processor it does not know, and this compiler knows only the names of the
    /// psABI levels, so any other name is taken to be a processor with what the unit already has.
    /// A `no-` in front of any of these is something gcc accepts and ignores.
    fn valued(
        &mut self,
        option: &str,
        key: &str,
        value: &str,
        negated: bool,
    ) -> Result<(), TargetRefusal> {
        let known = match key {
            "arch" => {
                if !negated {
                    self.arch = Isa::level(value);
                }
                true
            }
            "tune" => true,
            "fpmath" => ["387", "sse", "sse+387", "387+sse", "both"].contains(&value),
            "prefer-vector-width" => ["none", "128", "256", "512"].contains(&value),
            _ => return Err(TargetRefusal::Unknown(option.to_owned())),
        };
        if known { Ok(()) } else { Err(TargetRefusal::Value(option.to_owned())) }
    }

    /// The extensions the function is built for, given what the rest of the unit is built for.
    ///
    /// An `arch=` naming a level of the psABI adds that level's extensions to the unit's before
    /// the named extensions are applied. gcc starts from the processor alone and keeps only what
    /// the command line said explicitly, which this cannot tell apart from what `-march` supplied,
    /// so the unit's set is kept whole: the difference is only ever an extension the unit had and
    /// the named processor lacks.
    #[must_use]
    pub fn over(&self, unit: Isa) -> Isa {
        let base = self.arch.map_or(unit, |arch| arch.union(unit));
        self.choices.over(base)
    }
}

impl std::str::FromStr for Isa {
    type Err = String;

    /// The comma separated names [`Isa`]'s `Display` writes, each taken alone without what it is
    /// built over, which is what reading back a set written out needs.
    fn from_str(text: &str) -> Result<Isa, String> {
        let mut isa = Isa::NONE;
        for name in text.split(',').filter(|name| !name.is_empty()) {
            let feature =
                Feature::named(name).ok_or_else(|| format!("`{name}` is not an extension"))?;
            isa = isa.union(Isa(feature.bit()));
        }
        Ok(isa)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The macros a command line of `-m` flags defines over the baseline, sorted, which is how
    /// gcc 16.2.0's `-dM` output for the same line was compared.
    fn macros(flags: &[&str]) -> Vec<String> {
        let mut choices = Choices::new();
        for flag in flags {
            choices.read(flag).unwrap();
        }
        let mut out: Vec<String> = choices.over(Isa::baseline()).macros().collect();
        out.sort();
        out
    }

    /// The macro names, as a list a test can write.
    fn said(names: &[&str]) -> Vec<String> {
        let mut out: Vec<String> = names.iter().map(|name| format!("__{name}__")).collect();
        out.sort();
        out
    }

    #[test]
    fn every_feature_fits_in_the_set_and_every_need_is_a_feature() {
        assert!(FEATURES.len() <= 128);
        for row in FEATURES {
            for need in row.needs {
                assert!(Feature::named(need).is_some(), "{} needs {need}", row.name);
            }
        }
    }

    #[test]
    fn the_baseline_is_what_gcc_defines_with_no_flags() {
        assert_eq!(macros(&[]), said(&["FXSR", "MMX", "SSE", "SSE2"]));
    }

    #[test]
    fn a_level_brings_the_levels_under_it() {
        let base = said(&["FXSR", "MMX", "SSE", "SSE2"]);
        let with = |more: &[&str]| {
            let mut out = base.clone();
            out.extend(said(more));
            out.sort();
            out
        };
        assert_eq!(macros(&["sse3"]), with(&["SSE3"]));
        assert_eq!(macros(&["ssse3"]), with(&["SSE3", "SSSE3"]));
        assert_eq!(macros(&["sse4.1"]), with(&["SSE3", "SSSE3", "SSE4_1"]));
        let all = with(&["SSE3", "SSSE3", "SSE4_1", "SSE4_2", "POPCNT", "CRC32"]);
        assert_eq!(macros(&["sse4.2"]), all);
        assert_eq!(macros(&["sse4"]), all);
        assert_eq!(macros(&["popcnt"]), with(&["POPCNT"]));
        assert_eq!(macros(&["crc32"]), with(&["CRC32"]));
    }

    #[test]
    fn turning_a_level_off_takes_what_is_built_over_it() {
        let base = said(&["FXSR", "MMX", "SSE", "SSE2"]);
        assert_eq!(macros(&["sse4.2", "no-sse3"]), base);
        assert_eq!(macros(&["sse4", "no-sse4"]), base);
        let mut kept = said(&["SSE3", "SSSE3"]);
        kept.extend(base.clone());
        kept.sort();
        assert_eq!(macros(&["sse4.2", "no-sse4.1"]), kept);
    }

    #[test]
    fn a_flag_cancels_an_earlier_one_of_the_same_name() {
        let base = said(&["FXSR", "MMX", "SSE", "SSE2"]);
        assert_eq!(macros(&["sse4.2", "no-sse4.2"]), base);
        assert_eq!(macros(&["sse4.1", "no-sse4.1"]), base);
        // A different name is not cancelled, it is applied, and takes what it takes.
        let mut kept = said(&["SSE3", "SSSE3", "SSE4_1"]);
        kept.extend(base.clone());
        kept.sort();
        assert_eq!(macros(&["sse4.1", "no-sse4.2"]), kept);
        let mut kept = said(&["SSE3", "SSSE3"]);
        kept.extend(base);
        kept.sort();
        assert_eq!(macros(&["ssse3", "no-sse4.2"]), kept);
    }

    #[test]
    fn a_name_gcc_does_not_have_is_refused() {
        assert_eq!(Choices::new().read("sse5"), Err("sse5".to_owned()));
        assert_eq!(Choices::new().read("no-avx9"), Err("avx9".to_owned()));
        assert!(Choices::new().read("no-avx512bw").is_ok());
    }

    #[test]
    fn xsave_is_honoured_and_the_names_built_over_it_are_not_yet() {
        assert_eq!(macros(&["xsave"]), said(&["FXSR", "MMX", "SSE", "SSE2", "XSAVE"]));
        assert!(Feature::named("xsave").unwrap().honoured());
        assert!(!Feature::named("xsaveopt").unwrap().honoured());
    }

    #[test]
    fn the_count_and_the_checksum_follow_sse4_2_unless_they_were_mentioned() {
        let mut base = said(&["FXSR", "MMX", "SSE", "SSE2", "POPCNT"]);
        base.sort();
        assert_eq!(macros(&["popcnt", "sse4.2", "no-sse4.2"]), base);
        let no_count = macros(&["sse4.2", "no-popcnt"]);
        assert!(!no_count.contains(&"__POPCNT__".to_owned()));
        assert!(no_count.contains(&"__CRC32__".to_owned()));
        assert_eq!(macros(&["no-popcnt", "sse4.2"]), no_count);
        let no_step = macros(&["sse4.2", "no-crc32"]);
        assert!(no_step.contains(&"__POPCNT__".to_owned()));
        assert!(!no_step.contains(&"__CRC32__".to_owned()));
    }

    #[test]
    fn a_processor_supplies_only_what_nothing_else_said() {
        let v2 = Isa::level("x86-64-v2").unwrap();
        let mut told = Choices::new();
        told.read("no-sse3").unwrap();
        let mut got: Vec<String> = told.over(v2).macros().collect();
        got.sort();
        assert_eq!(got, said(&["FXSR", "MMX", "SSE", "SSE2", "POPCNT"]));
        let mut told = Choices::new();
        told.read("no-sse4.2").unwrap();
        let got: Vec<String> = told.over(v2).macros().collect();
        assert!(got.contains(&"__POPCNT__".to_owned()) && got.contains(&"__SSE4_1__".to_owned()));
        assert!(!got.contains(&"__CRC32__".to_owned()), "{got:?}");
    }

    #[test]
    fn a_level_above_what_is_honoured_defines_only_what_is() {
        let mut got: Vec<String> =
            Choices::new().over(Isa::level("x86-64-v3").unwrap()).macros().collect();
        got.sort();
        let want = said(&[
            "FXSR", "MMX", "SSE", "SSE2", "SSE3", "SSSE3", "SSE4_1", "SSE4_2", "POPCNT", "CRC32",
            "XSAVE",
        ]);
        assert_eq!(got, want);
        let v3 = Isa::level("x86-64-v3").unwrap();
        assert!(v3.has(Feature::named("avx2").unwrap()));
        assert!(v3.covers(Isa::level("x86-64-v2").unwrap()));
        assert!(!Isa::level("x86-64-v2").unwrap().covers(v3));
    }

    #[test]
    fn an_avx512_name_brings_the_avx_line_under_it() {
        let isa = Isa::with(Feature::named("avx512vpopcntdq").unwrap());
        for name in ["avx512f", "avx2", "avx", "sse4.2", "sse2"] {
            assert!(isa.has(Feature::named(name).unwrap()), "{name}");
        }
        assert!(!isa.has(Feature::named("avx512bw").unwrap()));
        assert_eq!(Feature::named("avx512vpopcntdq").unwrap().macro_name(), "__AVX512VPOPCNTDQ__");
        assert_eq!(Feature::named("amx-tile").unwrap().macro_name(), "__AMX_TILE__");
    }

    /// What one function's attribute strings make of a unit built for `unit`.
    fn attribute(strings: &[&str], unit: Isa) -> Result<Isa, TargetRefusal> {
        let mut target = Target::new();
        for text in strings {
            target.read(text)?;
        }
        Ok(target.over(unit))
    }

    #[test]
    fn an_attribute_string_is_a_list_over_the_unit() {
        let base = Isa::baseline();
        let sse42 = attribute(&["sse4.2"], base).unwrap();
        for name in ["sse4.1", "ssse3", "sse3", "popcnt", "crc32", "sse2"] {
            assert!(sse42.has(Feature::named(name).unwrap()), "{name}");
        }
        assert_eq!(
            attribute(&["sse4.2,no-popcnt"], base).unwrap(),
            sse42.minus(Isa::of(&["popcnt"]))
        );
        assert_eq!(attribute(&["sse4.2", "popcnt"], base).unwrap(), sse42);
        assert_eq!(attribute(&["popcnt"], base).unwrap(), base.union(Isa::of(&["popcnt"])));
        assert_eq!(
            attribute(&["no-sse4.2"], sse42).unwrap(),
            Isa::of(&["sse4.1", "mmx", "fxsr", "popcnt", "crc32"])
        );
        assert_eq!(attribute(&[""], base).unwrap(), base);
        assert_eq!(attribute(&["default"], sse42).unwrap(), sse42);
        let wide = attribute(&["avx512vpopcntdq,avx512bw"], base).unwrap();
        assert!(wide.has(Feature::named("avx512bw").unwrap()) && wide.covers(sse42));
    }

    #[test]
    fn an_attribute_arch_brings_a_level_and_other_options_change_nothing() {
        let base = Isa::baseline();
        let v2 = Isa::level("x86-64-v2").unwrap();
        assert!(attribute(&["arch=x86-64-v2"], base).unwrap().covers(v2));
        assert_eq!(attribute(&["arch=haswell,no-avx"], base).unwrap(), base);
        let plain = "tune=generic,fpmath=sse+387,prefer-vector-width=256,cld,no-cld,80387,\
                     general-regs-only,no-arch=x86-64,mwait";
        assert_eq!(attribute(&[plain], base).unwrap(), base.union(Isa::of(&["mwait"])));
    }

    #[test]
    fn an_attribute_string_gcc_refuses_is_refused_the_same_way() {
        let base = Isa::baseline();
        let unknown = |name: &str| Err(TargetRefusal::Unknown(name.to_owned()));
        assert_eq!(attribute(&["foo"], base), unknown("foo"));
        assert_eq!(attribute(&[" sse4.2"], base), unknown(" sse4.2"));
        assert_eq!(attribute(&["SSE4.2"], base), unknown("SSE4.2"));
        assert_eq!(attribute(&["sse4.2,,popcnt"], base), unknown(""));
        assert_eq!(attribute(&["branch-cost=3"], base), unknown("branch-cost=3"));
        assert_eq!(attribute(&["no-default"], base), unknown("no-default"));
        assert_eq!(attribute(&["default,sse4.2"], base), unknown("default"));
        assert_eq!(
            attribute(&["fpmath=bogus"], base),
            Err(TargetRefusal::Value("fpmath=bogus".to_owned()))
        );
        let negated = attribute(&["no-general-regs-only"], base).unwrap_err();
        assert_eq!(
            negated.to_string(),
            "pragma or attribute 'target(\"general-regs-only\")' does not allow a negated form"
        );
        assert_eq!(
            TargetRefusal::Unknown("foo".to_owned()).to_string(),
            "attribute 'target' argument 'foo' is unknown"
        );
    }

    #[test]
    fn a_set_written_out_reads_back_the_same() {
        let isa = Isa::level("x86-64-v3").unwrap().union(Isa::of(&["crc32"]));
        assert_eq!(isa.to_string().parse::<Isa>(), Ok(isa));
        assert_eq!("".parse::<Isa>(), Ok(Isa::NONE));
        assert!("sse9".parse::<Isa>().is_err());
    }
}
