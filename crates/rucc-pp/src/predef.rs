//! The predefined macro set, generated from the target description.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.5.
//!
//! The set is built as text and then read by the directive engine, which is what GCC does and
//! is not laziness. Constructing a few hundred `MacroDef` values by hand would need its own
//! parser for macro bodies, would not exercise the one that already exists, and could not be
//! read by a person checking a limit against the psABI. A file of `#define` lines can be
//! printed by `-dM`, diffed against GCC's output, and understood at a glance.
//!
//! Two synthetic files come out of this, and they are the two GCC names in a diagnostic:
//! `<built-in>` for the generated set and `<command-line>` for `-D` and `-U`. Keeping them
//! apart is what lets "`FOO` redefined" point at the command line rather than at a line
//! nobody wrote.
//!
//! The decision that everything else follows from is in section 4.5: we define `__GNUC__`,
//! which means glibc's headers, the kernel's headers and every autoconf probe take the GNU
//! path. The version claimed is deliberately conservative and is a knob, because claiming too
//! high a version means headers use extensions we do not have, and the matrix in `rucc-gnu`
//! is the list of promises the claim makes.

use rucc_session::OptLevel;
use rucc_target::{Arch, Env, Os, TargetInfo};

/// The name a diagnostic about the generated set points at.
pub const BUILT_IN: &str = "<built-in>";

/// The name a diagnostic about `-D` or `-U` points at.
pub const COMMAND_LINE: &str = "<command-line>";

/// Which C the source is written in.
///
/// The GNU variants are the same language with `__STRICT_ANSI__` left undefined, so the
/// dialect and the extension question are two fields rather than ten variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Std {
    /// `-std=c89`, and `-ansi`.
    C89,
    /// `-std=c99`.
    C99,
    /// `-std=c11`.
    C11,
    /// `-std=c17`, which is C11 with the defect reports applied.
    C17,
    /// `-std=c23`. The default, matching current GCC.
    #[default]
    C23,
}

impl Std {
    /// What `__STDC_VERSION__` says, which C89 does not define at all.
    pub const fn stdc_version(self) -> Option<&'static str> {
        match self {
            Std::C89 => None,
            Std::C99 => Some("199901L"),
            Std::C11 => Some("201112L"),
            Std::C17 => Some("201710L"),
            Std::C23 => Some("202311L"),
        }
    }

    /// The name in `-std=`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Std::C89 => "c89",
            Std::C99 => "c99",
            Std::C11 => "c11",
            Std::C17 => "c17",
            Std::C23 => "c23",
        }
    }

    /// Whether this dialect has `_Atomic`, `_Thread_local` and the rest of C11.
    const fn has_c11(self) -> bool {
        matches!(self, Std::C11 | Std::C17 | Std::C23)
    }
}

/// The GCC release the compiler claims to be.
///
/// Section 4.5 makes this a tunable, `-fgnuc-version=`, and says to start conservative and
/// raise it as the matrix in `rucc-gnu` fills in. The default is the version Clang claimed
/// for over a decade, which is the one value every real header set is known to cope with
/// from a compiler that is not GCC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GnucVersion {
    /// `__GNUC__`.
    pub major: u32,
    /// `__GNUC_MINOR__`.
    pub minor: u32,
    /// `__GNUC_PATCHLEVEL__`.
    pub patch: u32,
}

impl Default for GnucVersion {
    fn default() -> GnucVersion {
        GnucVersion { major: 4, minor: 2, patch: 1 }
    }
}

/// The translation date, as `__DATE__` and `__TIME__` spell it.
///
/// Fixed for the whole translation unit, which is what the standard requires and what makes
/// the two macros ordinary object-like macros rather than something the expander has to know
/// about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timestamp {
    /// `Mmm dd yyyy`, with the day space padded, which is the format the standard fixes.
    pub date: String,
    /// `hh:mm:ss`.
    pub time: String,
}

impl Timestamp {
    /// The current time, or `SOURCE_DATE_EPOCH` when the build asked for a reproducible one.
    ///
    /// Reading the environment here rather than in the driver is what GCC does, and it keeps
    /// the variable working for an embedder who never goes through a command line.
    pub fn now() -> Timestamp {
        let seconds = match std::env::var("SOURCE_DATE_EPOCH").ok().and_then(|v| v.parse().ok()) {
            Some(fixed) => fixed,
            None => std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs() as i64),
        };
        Timestamp::from_unix(seconds)
    }

    /// The time `seconds` after the epoch, in UTC.
    ///
    /// UTC rather than local time, because a compiler whose output depends on the machine's
    /// time zone is a compiler whose output is not reproducible.
    pub fn from_unix(seconds: i64) -> Timestamp {
        let days = seconds.div_euclid(86_400);
        let rest = seconds.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days);
        const MONTHS: [&str; 12] =
            ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
        let name = MONTHS[(month - 1) as usize];
        Timestamp {
            date: format!("{name} {day:2} {year}"),
            time: format!("{:02}:{:02}:{:02}", rest / 3600, (rest / 60) % 60, rest % 60),
        }
    }
}

/// The year, month and day `days` after 1970-01-01.
///
/// Howard Hinnant's civil calendar algorithm, which is a handful of divisions and no table.
/// It is here rather than in a dependency because the whole workspace has no dependencies,
/// and a date conversion is not a good reason to acquire the first one.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    // Shift the epoch to 0000-03-01, so that a leap day is the last day of the year and the
    // month lengths become a repeating pattern that one division can invert.
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let marched = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * marched + 2) / 5 + 1) as u32;
    let month = if marched < 10 { marched + 3 } else { marched - 9 } as u32;
    (year + i64::from(month <= 2), month, day)
}

/// Everything the predefined set is built from that is not the target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Predef {
    /// The dialect, which decides `__STDC_VERSION__`.
    pub std: Std,
    /// Whether the GNU extensions are on, which is `-std=gnu23` rather than `-std=c23`. It
    /// decides `__STRICT_ANSI__` and the unarmoured `linux` and `unix` macros.
    pub gnu_extensions: bool,
    /// The GCC release claimed.
    pub gnuc: GnucVersion,
    /// Decides `__OPTIMIZE__`, `__OPTIMIZE_SIZE__` and `__NO_INLINE__`.
    pub opt_level: OptLevel,
    /// Whether there is a standard library, which is `-ffreestanding` turned around.
    pub hosted: bool,
    /// `__DATE__` and `__TIME__`.
    pub timestamp: Timestamp,
    /// `-D` in command line order. `FOO` means `FOO=1`, as GCC has it.
    pub defines: Vec<String>,
    /// `-U` in command line order, applied after the defines.
    pub undefines: Vec<String>,
}

impl Predef {
    /// The default dialect, `gnu23`, at `-O0`.
    pub fn new() -> Predef {
        Predef {
            std: Std::default(),
            gnu_extensions: true,
            gnuc: GnucVersion::default(),
            opt_level: OptLevel::O0,
            hosted: true,
            timestamp: Timestamp::now(),
            defines: Vec::new(),
            undefines: Vec::new(),
        }
    }
}

impl Default for Predef {
    fn default() -> Predef {
        Predef::new()
    }
}

/// A file of `#define` lines being built up.
struct Defs {
    text: String,
}

impl Defs {
    fn new() -> Defs {
        Defs { text: String::new() }
    }

    /// `#define name value`.
    fn set(&mut self, name: &str, value: &str) {
        self.text.push_str("#define ");
        self.text.push_str(name);
        self.text.push(' ');
        self.text.push_str(value);
        self.text.push('\n');
    }

    /// `#define name 1`, which is what a macro that is only ever tested for needs.
    fn flag(&mut self, name: &str) {
        self.set(name, "1");
    }

    fn set_if(&mut self, when: bool, name: &str, value: &str) {
        if when {
            self.set(name, value);
        }
    }

    fn flag_if(&mut self, when: bool, name: &str) {
        if when {
            self.flag(name);
        }
    }
}

/// The whole predefined set for a target, as the text of a file.
pub(crate) fn built_in(target: &TargetInfo, opts: &Predef) -> String {
    let mut d = Defs::new();
    identity(&mut d, opts);
    // `__DATE__` and `__TIME__` are fixed for the whole translation unit, which is what the
    // standard asks for, so they are ordinary object-like macros and the expander needs to
    // know nothing about them.
    d.set("__DATE__", &format!("\"{}\"", opts.timestamp.date));
    d.set("__TIME__", &format!("\"{}\"", opts.timestamp.time));
    dialect(&mut d, opts);
    optimization(&mut d, opts);
    platform(&mut d, target, opts);
    sizes(&mut d, target);
    integers(&mut d, target);
    floats(&mut d, target);
    d.text
}

/// `-D` and `-U`, as the text of a file.
///
/// Empty when there are none, so that the caller can skip adding a file that would say
/// nothing. The undefines come last whatever order they were written in, because `-U` beats
/// `-D` in GCC no matter which side of it the `-D` was on.
pub(crate) fn command_line(opts: &Predef) -> String {
    let mut d = Defs::new();
    for define in &opts.defines {
        match define.split_once('=') {
            Some((name, value)) => d.set(name, value),
            // `-DFOO` is `-DFOO=1`. A macro nobody gave a value to is one that is only ever
            // tested for, and giving it an empty body would break `#if FOO`.
            None => d.flag(define),
        }
    }
    for name in &opts.undefines {
        d.text.push_str("#undef ");
        d.text.push_str(name);
        d.text.push('\n');
    }
    d.text
}

/// Who the compiler says it is.
fn identity(d: &mut Defs, opts: &Predef) {
    d.flag("__rucc__");
    d.set("__rucc_version__", "\"0.1.0\"");
    d.set("__rucc_major__", "0");
    d.set("__rucc_minor__", "1");
    d.set("__rucc_patchlevel__", "0");
    // The promise from section 4.5. Everything in the matrix hangs off this line.
    d.set("__GNUC__", &opts.gnuc.major.to_string());
    d.set("__GNUC_MINOR__", &opts.gnuc.minor.to_string());
    d.set("__GNUC_PATCHLEVEL__", &opts.gnuc.patch.to_string());
    d.set("__VERSION__", "\"rucc 0.1.0\"");
    // Not `__clang__`, deliberately. Section 4.5 says so, and a header that takes the Clang
    // path expects Clang's extension surface rather than GCC's.
    d.flag("__GNUC_STDC_INLINE__");
}

/// What the dialect flags say.
fn dialect(d: &mut Defs, opts: &Predef) {
    d.flag("__STDC__");
    d.set_if(opts.hosted, "__STDC_HOSTED__", "1");
    d.set_if(!opts.hosted, "__STDC_HOSTED__", "0");
    if let Some(version) = opts.std.stdc_version() {
        d.set("__STDC_VERSION__", version);
    }
    // Defined exactly when the extensions are off, which is the whole difference between
    // `-std=c23` and `-std=gnu23` as far as the preprocessor is concerned.
    d.flag_if(!opts.gnu_extensions, "__STRICT_ANSI__");
    d.flag("__STDC_UTF_16__");
    d.flag("__STDC_UTF_32__");
    d.flag("__STDC_IEC_559__");
    d.flag("__STDC_IEC_559_COMPLEX__");
    d.set_if(opts.std == Std::C23, "__STDC_IEC_60559_BFP__", "202311L");
    d.set("__STDC_ISO_10646__", "201706L");
    // C11 made these conditional features, and a header that sees `__STDC_VERSION__` at
    // 201112 with no `__STDC_NO_ATOMICS__` next to it will use `_Atomic`.
    if opts.std.has_c11() {
        d.flag("__STDC_NO_ATOMICS__");
        d.flag("__STDC_NO_THREADS__");
        d.flag("__STDC_NO_COMPLEX__");
        d.flag("__STDC_NO_VLA__");
    }
}

/// What the optimizer level says.
fn optimization(d: &mut Defs, opts: &Predef) {
    d.flag_if(opts.opt_level.runs_optimizer(), "__OPTIMIZE__");
    d.flag_if(opts.opt_level.is_size(), "__OPTIMIZE_SIZE__");
    // glibc's headers test this before deciding whether to define a function as an inline
    // wrapper, so getting it wrong changes what a program links against.
    d.flag_if(!opts.opt_level.runs_optimizer(), "__NO_INLINE__");
}

/// The architecture, the operating system and the object format.
fn platform(d: &mut Defs, target: &TargetInfo, opts: &Predef) {
    let triple = target.triple;
    match triple.arch {
        Arch::X86_64 => {
            d.flag("__x86_64__");
            d.flag("__x86_64");
            d.flag("__amd64__");
            d.flag("__amd64");
            d.flag("__SSE__");
            d.flag("__SSE2__");
            d.flag("__MMX__");
            d.flag("__SSE_MATH__");
            d.flag("__SSE2_MATH__");
            d.flag("__k8");
            d.flag("__k8__");
        }
        Arch::Aarch64 => {
            d.flag("__aarch64__");
            d.flag("__AARCH64EL__");
            d.set("__ARM_ARCH", "8");
            d.set("__ARM_ARCH_PROFILE", "'A'");
            d.set("__ARM_64BIT_STATE", "1");
            d.set("__ARM_ALIGN_MAX_PWR", "28");
            d.set("__ARM_FP", "0xe");
            d.set("__ARM_NEON", "1");
            d.set("__ARM_FEATURE_UNALIGNED", "1");
            d.set("__ARM_PCS_AAPCS64", "1");
        }
        Arch::Riscv64 => {
            d.flag("__riscv");
            d.set("__riscv_xlen", "64");
            d.set("__riscv_flen", "64");
            d.flag("__riscv_float_abi_double");
            d.flag("__riscv_muldiv");
            d.flag("__riscv_atomic");
            d.flag("__riscv_compressed");
            d.set("__riscv_cmodel_medlow", "1");
        }
    }
    match triple.os {
        Os::Linux => {
            d.flag("__linux__");
            d.flag("__linux");
            d.flag("__unix__");
            d.flag("__unix");
            d.flag("__gnu_linux__");
            d.flag("__ELF__");
            // The unarmoured spellings are not reserved identifiers, so a strict mode may not
            // define them. Autoconf still tests for `linux`, which is why they exist at all.
            if opts.gnu_extensions {
                d.flag("linux");
                d.flag("unix");
            }
        }
        Os::Darwin => {
            d.flag("__APPLE__");
            d.flag("__MACH__");
            d.flag("__unix__");
            d.flag("__unix");
            d.set("__APPLE_CC__", "6000");
            d.set("__DYNAMIC__", "1");
            if opts.gnu_extensions {
                d.flag("unix");
            }
        }
        Os::Windows => {
            d.flag("_WIN32");
            d.flag("__WIN32__");
            d.flag("_WIN64");
            d.flag("__WIN64__");
            d.flag("__MINGW32__");
        }
        Os::None => {
            // Freestanding. `__ELF__` still holds, because the object format is a property of
            // the target rather than of having an operating system under it.
            d.flag("__ELF__");
        }
    }
    match triple.env {
        Env::Musl => d.flag("__musl__"),
        Env::Gnu | Env::None | Env::Msvc => {}
    }
    // LP64 is the model everywhere except Windows, and a great deal of code tests for it
    // rather than testing pointer and long widths separately.
    if target.long_width == 64 && target.pointer_width == 64 {
        d.flag("__LP64__");
        d.flag("_LP64");
    }
    // Position independent code is the default on the ELF targets and on Apple's, which is
    // what a distribution build expects. The value 2 is GCC's for `-fPIC` rather than `-fpic`.
    if !matches!(triple.os, Os::Windows) {
        d.set("__PIC__", "2");
        d.set("__pic__", "2");
    }
}

/// `__CHAR_BIT__`, the `__SIZEOF_*__` family and the alignment macros.
fn sizes(d: &mut Defs, target: &TargetInfo) {
    let pointer = target.pointer_width / 8;
    let long = target.long_width / 8;
    let long_double = target.long_double_width / 8;
    d.set("__CHAR_BIT__", "8");
    d.set("__SIZEOF_SHORT__", "2");
    d.set("__SIZEOF_INT__", "4");
    d.set("__SIZEOF_LONG__", &long.to_string());
    d.set("__SIZEOF_LONG_LONG__", "8");
    d.set("__SIZEOF_INT128__", "16");
    d.set("__SIZEOF_FLOAT__", "4");
    d.set("__SIZEOF_DOUBLE__", "8");
    d.set("__SIZEOF_LONG_DOUBLE__", &long_double.to_string());
    d.set("__SIZEOF_POINTER__", &pointer.to_string());
    d.set("__SIZEOF_SIZE_T__", &pointer.to_string());
    d.set("__SIZEOF_PTRDIFF_T__", &pointer.to_string());
    d.set("__SIZEOF_WCHAR_T__", &wchar(target).size.to_string());
    d.set("__SIZEOF_WINT_T__", "4");
    d.set("__BIGGEST_ALIGNMENT__", "16");
    // The `__BYTE_ORDER__` family, which the kernel and every serialisation library read.
    // The names of the orders are defined whichever one is in force, because code compares
    // against both.
    d.set("__ORDER_LITTLE_ENDIAN__", "1234");
    d.set("__ORDER_BIG_ENDIAN__", "4321");
    d.set("__ORDER_PDP_ENDIAN__", "3412");
    let order =
        if target.little_endian { "__ORDER_LITTLE_ENDIAN__" } else { "__ORDER_BIG_ENDIAN__" };
    d.set("__BYTE_ORDER__", order);
    d.set("__FLOAT_WORD_ORDER__", order);
    d.flag_if(!target.char_is_signed, "__CHAR_UNSIGNED__");
}

/// How `wchar_t` is spelled on a target, and what it holds.
struct Wchar {
    /// The C type it is a name for.
    spelling: &'static str,
    /// Its width in bytes.
    size: u32,
    /// `__WCHAR_MAX__`.
    max: &'static str,
    /// `__WCHAR_MIN__`.
    min: &'static str,
}

/// `wchar_t` is the type that divides the targets most and is written down least.
///
/// Windows makes it 16 bits so that a wide string is UTF-16. AArch64 Linux makes it unsigned,
/// following the psABI's rule for plain `char`, while x86-64 Linux makes it signed. Code that
/// compares a `wchar_t` against a negative value is correct on one and not on the other.
fn wchar(target: &TargetInfo) -> Wchar {
    match (target.triple.arch, target.triple.os) {
        (_, Os::Windows) => {
            Wchar { spelling: "short unsigned int", size: 2, max: "0xffff", min: "0" }
        }
        (Arch::Aarch64, Os::Linux | Os::None) => {
            Wchar { spelling: "unsigned int", size: 4, max: "0xffffffffU", min: "0U" }
        }
        _ => Wchar { spelling: "int", size: 4, max: "0x7fffffff", min: "(-__WCHAR_MAX__ - 1)" },
    }
}

/// The integer type names, their limits, and the exact width family.
fn integers(d: &mut Defs, target: &TargetInfo) {
    // The one fact everything below turns on: which type is 64 bits wide. On LP64 it is
    // `long`, and on Windows LLP64 it is `long long`, and every `size_t`, `intmax_t` and
    // `int64_t` spelling follows from that.
    let lp64 = target.long_width == 64;
    let wide = if lp64 { "long int" } else { "long long int" };
    let wide_unsigned = if lp64 { "long unsigned int" } else { "long long unsigned int" };
    let wide_suffix = if lp64 { "L" } else { "LL" };
    let wide_max = format!("9223372036854775807{wide_suffix}");
    let wide_umax = format!("18446744073709551615U{wide_suffix}");

    d.set("__SCHAR_MAX__", "127");
    d.set("__SHRT_MAX__", "32767");
    d.set("__INT_MAX__", "2147483647");
    d.set("__LONG_MAX__", if lp64 { "9223372036854775807L" } else { "2147483647L" });
    d.set("__LONG_LONG_MAX__", "9223372036854775807LL");
    d.set("__INTMAX_MAX__", &wide_max);
    d.set("__UINTMAX_MAX__", &wide_umax);
    d.set("__SIZE_MAX__", &wide_umax);
    d.set("__PTRDIFF_MAX__", &wide_max);
    d.set("__INTPTR_MAX__", &wide_max);
    d.set("__UINTPTR_MAX__", &wide_umax);
    d.set("__SIG_ATOMIC_MAX__", "2147483647");
    d.set("__SIG_ATOMIC_MIN__", "(-__SIG_ATOMIC_MAX__ - 1)");
    d.set("__WINT_MAX__", "4294967295U");
    d.set("__WINT_MIN__", "0U");

    let wchar = wchar(target);
    d.set("__WCHAR_TYPE__", wchar.spelling);
    d.set("__WCHAR_MAX__", wchar.max);
    d.set("__WCHAR_MIN__", wchar.min);
    d.set("__WINT_TYPE__", "unsigned int");
    d.set("__SIZE_TYPE__", wide_unsigned);
    d.set("__PTRDIFF_TYPE__", wide);
    d.set("__INTMAX_TYPE__", wide);
    d.set("__UINTMAX_TYPE__", wide_unsigned);
    d.set("__INTPTR_TYPE__", wide);
    d.set("__UINTPTR_TYPE__", wide_unsigned);
    d.set("__SIG_ATOMIC_TYPE__", "int");
    d.set("__CHAR16_TYPE__", "short unsigned int");
    d.set("__CHAR32_TYPE__", "unsigned int");
    d.set("__INTMAX_C(c)", &format!("c ## {wide_suffix}"));
    d.set("__UINTMAX_C(c)", &format!("c ## U{wide_suffix}"));

    // The exact width family, which is what a freestanding `stdint.h` is written out of.
    exact(d, 8, "signed char", "unsigned char", "127", "255", "");
    exact(d, 16, "short int", "short unsigned int", "32767", "65535", "");
    exact(d, 32, "int", "unsigned int", "2147483647", "4294967295U", "U");
    exact(d, 64, wide, wide_unsigned, &wide_max, &wide_umax, wide_suffix);

    // The fast types. GCC makes the 16 and 32 bit ones `long` on x86-64 and `int` elsewhere,
    // and a header that computes a printf format from the type name notices the difference.
    let fast_middle = if target.triple.arch == Arch::X86_64 && lp64 { wide } else { "int" };
    d.set("__INT_FAST8_TYPE__", "signed char");
    d.set("__UINT_FAST8_TYPE__", "unsigned char");
    d.set("__INT_FAST8_MAX__", "127");
    d.set("__UINT_FAST8_MAX__", "255");
    for width in [16, 32] {
        let unsigned = if fast_middle == "int" { "unsigned int" } else { wide_unsigned };
        let max = if fast_middle == "int" { "2147483647" } else { wide_max.as_str() };
        let umax = if fast_middle == "int" { "4294967295U" } else { wide_umax.as_str() };
        d.set(&format!("__INT_FAST{width}_TYPE__"), fast_middle);
        d.set(&format!("__UINT_FAST{width}_TYPE__"), unsigned);
        d.set(&format!("__INT_FAST{width}_MAX__"), max);
        d.set(&format!("__UINT_FAST{width}_MAX__"), umax);
    }
    d.set("__INT_FAST64_TYPE__", wide);
    d.set("__UINT_FAST64_TYPE__", wide_unsigned);
    d.set("__INT_FAST64_MAX__", &wide_max);
    d.set("__UINT_FAST64_MAX__", &wide_umax);
}

/// One width of the exact and least families, which are the same types.
fn exact(
    d: &mut Defs,
    width: u32,
    signed: &str,
    unsigned: &str,
    max: &str,
    umax: &str,
    suffix: &str,
) {
    d.set(&format!("__INT{width}_TYPE__"), signed);
    d.set(&format!("__UINT{width}_TYPE__"), unsigned);
    d.set(&format!("__INT{width}_MAX__"), max);
    d.set(&format!("__UINT{width}_MAX__"), umax);
    d.set(&format!("__INT_LEAST{width}_TYPE__"), signed);
    d.set(&format!("__UINT_LEAST{width}_TYPE__"), unsigned);
    d.set(&format!("__INT_LEAST{width}_MAX__"), max);
    d.set(&format!("__UINT_LEAST{width}_MAX__"), umax);
    // The constant makers. `__INT8_C(1)` is `1` and not `1 ## `, because a paste with nothing
    // on the right is not a token the expander should have to think about.
    if suffix.is_empty() {
        d.set(&format!("__INT{width}_C(c)"), "c");
        d.set(&format!("__UINT{width}_C(c)"), "c ## U");
    } else {
        d.set(&format!("__INT{width}_C(c)"), &format!("c ## {suffix}"));
        d.set(&format!("__UINT{width}_C(c)"), &format!("c ## U{suffix}"));
    }
}

/// The `float.h` characteristics.
fn floats(d: &mut Defs, target: &TargetInfo) {
    d.set("__FLT_RADIX__", "2");
    d.set("__FLT_EVAL_METHOD__", "0");
    d.set("__FLT_MANT_DIG__", "24");
    d.set("__FLT_DIG__", "6");
    d.set("__FLT_MIN_EXP__", "(-125)");
    d.set("__FLT_MIN_10_EXP__", "(-37)");
    d.set("__FLT_MAX_EXP__", "128");
    d.set("__FLT_MAX_10_EXP__", "38");
    d.set("__FLT_DECIMAL_DIG__", "9");
    d.set("__FLT_MAX__", "3.40282346638528859811704183484516925e+38F");
    d.set("__FLT_MIN__", "1.17549435082228750796873653722224568e-38F");
    d.set("__FLT_EPSILON__", "1.19209289550781250000000000000000000e-7F");
    d.set("__FLT_DENORM_MIN__", "1.40129846432481707092372958328991613e-45F");
    d.set("__FLT_HAS_DENORM__", "1");
    d.set("__FLT_HAS_INFINITY__", "1");
    d.set("__FLT_HAS_QUIET_NAN__", "1");

    d.set("__DBL_MANT_DIG__", "53");
    d.set("__DBL_DIG__", "15");
    d.set("__DBL_MIN_EXP__", "(-1021)");
    d.set("__DBL_MIN_10_EXP__", "(-307)");
    d.set("__DBL_MAX_EXP__", "1024");
    d.set("__DBL_MAX_10_EXP__", "308");
    d.set("__DBL_DECIMAL_DIG__", "17");
    d.set("__DBL_MAX__", "((double)1.79769313486231570814527423731704357e+308L)");
    d.set("__DBL_MIN__", "((double)2.22507385850720138309023271733240406e-308L)");
    d.set("__DBL_EPSILON__", "((double)2.22044604925031308084726333618164062e-16L)");
    d.set("__DBL_DENORM_MIN__", "((double)4.94065645841246544176568792868221372e-324L)");
    d.set("__DBL_HAS_DENORM__", "1");
    d.set("__DBL_HAS_INFINITY__", "1");
    d.set("__DBL_HAS_QUIET_NAN__", "1");

    long_double(d, target);
    d.set("__DECIMAL_DIG__", "__LDBL_DECIMAL_DIG__");
}

/// `long double` is three different types across the targets, and the macros have to say so.
///
/// On SysV x86-64 it is 80 bits of x87 stored in 16 bytes. On AArch64 and RISC-V Linux it is
/// true quad precision. On Apple platforms and Windows it is `double` under another name.
/// `spec/12-abi-and-runtime.md` section 12.3 has the ABI consequences.
fn long_double(d: &mut Defs, target: &TargetInfo) {
    if target.long_double_width == 64 {
        d.set("__LDBL_MANT_DIG__", "53");
        d.set("__LDBL_DIG__", "15");
        d.set("__LDBL_MIN_EXP__", "(-1021)");
        d.set("__LDBL_MIN_10_EXP__", "(-307)");
        d.set("__LDBL_MAX_EXP__", "1024");
        d.set("__LDBL_MAX_10_EXP__", "308");
        d.set("__LDBL_DECIMAL_DIG__", "17");
        d.set("__LDBL_MAX__", "1.79769313486231570814527423731704357e+308L");
        d.set("__LDBL_MIN__", "2.22507385850720138309023271733240406e-308L");
        d.set("__LDBL_EPSILON__", "2.22044604925031308084726333618164062e-16L");
        d.set("__LDBL_DENORM_MIN__", "4.94065645841246544176568792868221372e-324L");
    } else if target.triple.arch == Arch::X86_64 {
        d.set("__LDBL_MANT_DIG__", "64");
        d.set("__LDBL_DIG__", "18");
        d.set("__LDBL_MIN_EXP__", "(-16381)");
        d.set("__LDBL_MIN_10_EXP__", "(-4931)");
        d.set("__LDBL_MAX_EXP__", "16384");
        d.set("__LDBL_MAX_10_EXP__", "4932");
        d.set("__LDBL_DECIMAL_DIG__", "21");
        d.set("__LDBL_MAX__", "1.18973149535723176502126385303097021e+4932L");
        d.set("__LDBL_MIN__", "3.36210314311209350626267781732175260e-4932L");
        d.set("__LDBL_EPSILON__", "1.08420217248550443400745280086994171e-19L");
        d.set("__LDBL_DENORM_MIN__", "3.64519953188247460252840593361941982e-4951L");
    } else {
        d.set("__LDBL_MANT_DIG__", "113");
        d.set("__LDBL_DIG__", "33");
        d.set("__LDBL_MIN_EXP__", "(-16381)");
        d.set("__LDBL_MIN_10_EXP__", "(-4931)");
        d.set("__LDBL_MAX_EXP__", "16384");
        d.set("__LDBL_MAX_10_EXP__", "4932");
        d.set("__LDBL_DECIMAL_DIG__", "36");
        d.set("__LDBL_MAX__", "1.18973149535723176508575932662800702e+4932L");
        d.set("__LDBL_MIN__", "3.36210314311209350626267781732175260e-4932L");
        d.set("__LDBL_EPSILON__", "1.92592994438723585305597794258492732e-34L");
        d.set("__LDBL_DENORM_MIN__", "6.47517511943802511092443895822764655e-4966L");
    }
    d.set("__LDBL_HAS_DENORM__", "1");
    d.set("__LDBL_HAS_INFINITY__", "1");
    d.set("__LDBL_HAS_QUIET_NAN__", "1");
}

#[cfg(test)]
mod tests {
    use rucc_target::Triple;

    use super::*;

    fn set_for(triple: &str) -> String {
        let triple: Triple = triple.parse().expect("a triple the compiler supports");
        built_in(&TargetInfo::new(triple), &Predef::new())
    }

    fn has(text: &str, line: &str) -> bool {
        text.lines().any(|l| l == line)
    }

    #[test]
    fn the_set_is_driven_by_the_target_rather_than_by_the_host() {
        let x86 = set_for("x86_64-unknown-linux-gnu");
        let arm = set_for("aarch64-unknown-linux-gnu");
        assert!(has(&x86, "#define __x86_64__ 1"));
        assert!(!has(&x86, "#define __aarch64__ 1"));
        assert!(has(&arm, "#define __aarch64__ 1"));
        assert!(!has(&arm, "#define __x86_64__ 1"));
        assert!(has(&x86, "#define __linux__ 1") && has(&arm, "#define __linux__ 1"));
    }

    #[test]
    fn windows_is_the_target_that_makes_long_thirty_two_bits() {
        let windows = set_for("x86_64-pc-windows-msvc");
        let linux = set_for("x86_64-unknown-linux-gnu");
        assert!(has(&windows, "#define __SIZEOF_LONG__ 4"));
        assert!(has(&windows, "#define __SIZE_TYPE__ long long unsigned int"));
        assert!(has(&windows, "#define __INT64_TYPE__ long long int"));
        assert!(!has(&windows, "#define __LP64__ 1"));
        assert!(has(&linux, "#define __SIZEOF_LONG__ 8"));
        assert!(has(&linux, "#define __SIZE_TYPE__ long unsigned int"));
        assert!(has(&linux, "#define __INT64_TYPE__ long int"));
        assert!(has(&linux, "#define __LP64__ 1"));
    }

    #[test]
    fn wchar_t_is_the_type_that_divides_the_targets() {
        // Signed on x86-64 Linux, unsigned on AArch64 Linux, and sixteen bits on Windows.
        assert!(has(&set_for("x86_64-unknown-linux-gnu"), "#define __WCHAR_TYPE__ int"));
        assert!(has(&set_for("aarch64-unknown-linux-gnu"), "#define __WCHAR_TYPE__ unsigned int"));
        let windows = set_for("x86_64-pc-windows-msvc");
        assert!(has(&windows, "#define __WCHAR_TYPE__ short unsigned int"));
        assert!(has(&windows, "#define __SIZEOF_WCHAR_T__ 2"));
    }

    #[test]
    fn long_double_is_three_types_and_the_macros_say_which() {
        assert!(has(&set_for("x86_64-unknown-linux-gnu"), "#define __LDBL_MANT_DIG__ 64"));
        assert!(has(&set_for("aarch64-unknown-linux-gnu"), "#define __LDBL_MANT_DIG__ 113"));
        assert!(has(&set_for("aarch64-apple-darwin"), "#define __LDBL_MANT_DIG__ 53"));
    }

    #[test]
    fn char_signedness_is_recorded_only_when_it_is_unsigned() {
        // Which is how GCC does it: the macro exists to mark the unusual case.
        assert!(has(&set_for("aarch64-unknown-linux-gnu"), "#define __CHAR_UNSIGNED__ 1"));
        assert!(!has(&set_for("x86_64-unknown-linux-gnu"), "#define __CHAR_UNSIGNED__ 1"));
    }

    #[test]
    fn the_dialect_decides_the_standard_macros() {
        let mut opts = Predef::new();
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse().unwrap());
        assert!(has(&built_in(&target, &opts), "#define __STDC_VERSION__ 202311L"));
        assert!(!has(&built_in(&target, &opts), "#define __STRICT_ANSI__ 1"));
        assert!(has(&built_in(&target, &opts), "#define linux 1"));

        opts.gnu_extensions = false;
        assert!(has(&built_in(&target, &opts), "#define __STRICT_ANSI__ 1"));
        assert!(!has(&built_in(&target, &opts), "#define linux 1"), "not a reserved name");

        opts.std = Std::C89;
        let c89 = built_in(&target, &opts);
        assert!(!c89.contains("__STDC_VERSION__"), "C89 does not define it at all");
        assert!(has(&c89, "#define __STDC__ 1"));
    }

    #[test]
    fn the_optimizer_level_is_visible_to_the_preprocessor() {
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse().unwrap());
        let mut opts = Predef::new();
        assert!(has(&built_in(&target, &opts), "#define __NO_INLINE__ 1"));
        assert!(!built_in(&target, &opts).contains("__OPTIMIZE__"));

        opts.opt_level = OptLevel::O2;
        assert!(has(&built_in(&target, &opts), "#define __OPTIMIZE__ 1"));
        assert!(!built_in(&target, &opts).contains("__OPTIMIZE_SIZE__"));

        opts.opt_level = OptLevel::Os;
        assert!(has(&built_in(&target, &opts), "#define __OPTIMIZE_SIZE__ 1"));
    }

    #[test]
    fn a_command_line_define_with_no_value_is_one() {
        let mut opts = Predef::new();
        opts.defines = vec!["FOO".to_owned(), "BAR=2".to_owned(), "F(x)=x + 1".to_owned()];
        opts.undefines = vec!["__linux__".to_owned()];
        let text = command_line(&opts);
        assert!(has(&text, "#define FOO 1"));
        assert!(has(&text, "#define BAR 2"));
        assert!(has(&text, "#define F(x) x + 1"));
        // The undefine comes last, because `-U` beats `-D` whichever side of it it was on.
        assert!(text.trim_end().ends_with("#undef __linux__"));
    }

    #[test]
    fn no_command_line_macros_is_no_file_at_all() {
        assert!(command_line(&Predef::new()).is_empty());
    }

    #[test]
    fn a_date_is_spelled_the_way_the_standard_fixes() {
        // The epoch itself, and a day that needs the space padding the format asks for.
        let epoch = Timestamp::from_unix(0);
        assert_eq!(epoch.date, "Jan  1 1970");
        assert_eq!(epoch.time, "00:00:00");
        let leap = Timestamp::from_unix(1_709_164_800);
        assert_eq!(leap.date, "Feb 29 2024", "2024 is a leap year");
        let late = Timestamp::from_unix(1_735_689_599);
        assert_eq!(late.date, "Dec 31 2024");
        assert_eq!(late.time, "23:59:59");
    }

    #[test]
    fn a_date_before_the_epoch_still_comes_out_right() {
        // Not because anyone compiles in 1969, but because the arithmetic that gets this
        // wrong is the same arithmetic that gets a time zone offset wrong.
        assert_eq!(Timestamp::from_unix(-1).date, "Dec 31 1969");
        assert_eq!(Timestamp::from_unix(-1).time, "23:59:59");
    }

    #[test]
    fn the_gnuc_version_is_a_knob_rather_than_a_constant() {
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse().unwrap());
        let mut opts = Predef::new();
        assert!(has(&built_in(&target, &opts), "#define __GNUC__ 4"));
        opts.gnuc = GnucVersion { major: 15, minor: 1, patch: 0 };
        assert!(has(&built_in(&target, &opts), "#define __GNUC__ 15"));
        assert!(has(&built_in(&target, &opts), "#define __GNUC_MINOR__ 1"));
    }
}
