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

use rucc_base::float::Format;
use rucc_session::{GnucVersion, OptLevel, Options, Std};
use rucc_target::{Arch, Env, Os, TargetInfo};

/// The name a diagnostic about the generated set points at.
pub const BUILT_IN: &str = "<built-in>";

/// The name a diagnostic about `-D` or `-U` points at.
pub const COMMAND_LINE: &str = "<command-line>";

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

impl Predef {
    /// The set the command line asked for.
    ///
    /// The mapping lives here rather than in the driver because it is the definition of what
    /// each flag means to the macro set, and the driver's job is to parse a command line, not
    /// to know that `-ffreestanding` is `__STDC_HOSTED__` being zero.
    pub fn for_options(opts: &Options) -> Predef {
        Predef {
            std: opts.std,
            gnu_extensions: opts.gnu_extensions,
            gnuc: opts.gnuc,
            opt_level: opts.opt_level,
            hosted: opts.hosted,
            timestamp: Timestamp::now(),
            defines: opts.defines.clone(),
            undefines: opts.undefines.clone(),
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
    atomics(&mut d, target);
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
    // The type behind `char8_t`, which C23 added and no dialect before it has. It sits here
    // rather than next to `__CHAR16_TYPE__` and `__CHAR32_TYPE__` because those two are the
    // same in every dialect and this one is not, which is the whole reason a header can test
    // for it: gcc's own `stdatomic.h` writes `atomic_char8_t` under `#ifdef __CHAR8_TYPE__`
    // and gets it in C23 and not in C17.
    d.set_if(opts.std == Std::C23, "__CHAR8_TYPE__", "unsigned char");
    // C11 made these conditional features, and a header that sees `__STDC_VERSION__` at
    // 201112 with no `__STDC_NO_ATOMICS__` next to it will use `_Atomic`. Each one here is a
    // claim not to have something, so each one is only correct while it stays true: atomics
    // because there is no `stdatomic.h` to include, threads because there is no `threads.h`,
    // and complex because the arithmetic is not lowered.
    //
    // Variable length arrays are not on this list, because they work. Claiming otherwise is
    // not a harmless overstatement of caution: glibc's `regex.h` writes the bound of
    // `regexec`'s match array as `_REGEX_NELTS (__nmatch)`, which is the parameter when the
    // dialect has them and nothing at all when a compiler says it does not, so the claim
    // silently changes a declaration in a header rather than turning something off.
    if opts.std.has_c11() {
        d.flag("__STDC_NO_ATOMICS__");
        d.flag("__STDC_NO_THREADS__");
        d.flag("__STDC_NO_COMPLEX__");
    }
    // What `__has_embed` answers with. They are defined in every dialect and not only in C23,
    // because the operator is answerable in every dialect and a header that writes
    // `#if __has_embed(...) == __STDC_EMBED_FOUND__` under `-std=gnu17` would otherwise be
    // comparing against zero and taking the not found branch on a resource that is there.
    d.set("__STDC_EMBED_NOT_FOUND__", "0");
    d.set("__STDC_EMBED_FOUND__", "1");
    d.set("__STDC_EMBED_EMPTY__", "2");
}

/// The memory orders and the lock free answers.
///
/// These are here whether or not `_Atomic` is, and `__STDC_NO_ATOMICS__` does not turn them
/// off, because they are the numbering the `__atomic` builtins take rather than a promise
/// about the language. musl's `stdatomic.h` writes `memory_order_relaxed = __ATOMIC_RELAXED`
/// with no test around it at all, so a compiler without them prints an enumerator whose value
/// is an identifier.
///
/// Two means always lock free, and every integer type gets a two on all three targets, which
/// are all sixty four bit machines. `long long` is the one that would change on a thirty two
/// bit target, where a double word load is an instruction the machine may or may not have.
fn atomics(d: &mut Defs, target: &TargetInfo) {
    d.set("__ATOMIC_RELAXED", "0");
    d.set("__ATOMIC_CONSUME", "1");
    d.set("__ATOMIC_ACQUIRE", "2");
    d.set("__ATOMIC_RELEASE", "3");
    d.set("__ATOMIC_ACQ_REL", "4");
    d.set("__ATOMIC_SEQ_CST", "5");
    // The gate is the machine word rather than `long`, because Windows has a thirty two bit
    // `long` on a sixty four bit machine and its `long long` is still one instruction.
    let llong = if target.pointer_width == 64 { "2" } else { "1" };
    for name in [
        "BOOL", "CHAR", "CHAR8_T", "CHAR16_T", "CHAR32_T", "WCHAR_T", "SHORT", "INT", "LONG",
        "POINTER",
    ] {
        d.set(&format!("__GCC_ATOMIC_{name}_LOCK_FREE"), "2");
    }
    // The one that is not always two: a target whose word is thirty two bits wide can only
    // promise `long long` is lock free if it has a double word instruction, and the honest
    // answer there is sometimes rather than always.
    d.set("__GCC_ATOMIC_LLONG_LOCK_FREE", llong);
    d.set("__GCC_ATOMIC_TEST_AND_SET_TRUEVAL", "1");
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
            if triple.arch == Arch::Aarch64 {
                // Apple's own spelling of the architecture, which its headers use rather than
                // __aarch64__. sys/cdefs.h tests for it by name and reaches an #error called
                // "Unsupported architecture" without it, so every system header on this
                // platform fails on the first include until these two are here.
                d.flag("__arm64__");
                d.flag("__arm64");
            }
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
    // What the assembler prepends to a C name to get the symbol. Mach-O keeps the leading
    // underscore that every a.out toolchain had and ELF dropped it. It has to be defined even
    // where it is empty, because of how it is used: glibc writes `__asm__ (__ASMNAME (name))`
    // and that stringifies `__USER_LABEL_PREFIX__`, so a compiler that leaves it undefined
    // does not get an error, it gets the name of the macro as the string and renames the
    // function.
    d.set("__USER_LABEL_PREFIX__", if triple.os == Os::Darwin { "_" } else { "" });

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
///
/// The width and the signedness come from the target description rather than from another match
/// on the triple, because the lexer needs the same two facts to convert a wide literal and the
/// two answers have to be the same one.
fn wchar(target: &TargetInfo) -> Wchar {
    match (target.wchar_width, target.wchar_is_signed) {
        (16, false) => Wchar { spelling: "short unsigned int", size: 2, max: "0xffff", min: "0" },
        (16, true) => Wchar { spelling: "short int", size: 2, max: "0x7fff", min: "(-32767 - 1)" },
        (_, false) => Wchar { spelling: "unsigned int", size: 4, max: "0xffffffffU", min: "0U" },
        (_, true) => {
            Wchar { spelling: "int", size: 4, max: "0x7fffffff", min: "(-__WCHAR_MAX__ - 1)" }
        }
    }
}

/// How `wint_t` is spelled on a target, and what it holds.
struct Wint {
    /// The C type it is a name for.
    spelling: &'static str,
    /// `__WINT_MAX__`.
    max: &'static str,
    /// `__WINT_MIN__`.
    min: &'static str,
    /// `__WINT_WIDTH__`, which follows the spelling rather than `__SIZEOF_WINT_T__`.
    width: u32,
}

/// `wint_t` does not follow `wchar_t`, and Darwin is where that shows.
///
/// Apple makes it a signed `int`, so that `WEOF` is negative the way `EOF` is, while Linux
/// makes it `unsigned int` and gives `WEOF` the value `0xffffffff`. The SDK's `arm/_types.h`
/// spells `__darwin_wint_t` as `__WINT_TYPE__` and nothing else, so getting this wrong changes
/// the signedness of every wide character function's argument on that platform.
fn wint(target: &TargetInfo) -> Wint {
    match target.triple.os {
        Os::Windows => Wint { spelling: "short unsigned int", max: "0xffff", min: "0", width: 16 },
        Os::Darwin => {
            Wint { spelling: "int", max: "0x7fffffff", min: "(-__WINT_MAX__ - 1)", width: 32 }
        }
        _ => Wint { spelling: "unsigned int", max: "0xffffffffU", min: "0U", width: 32 },
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
    let wide_max = format!("0x7fffffffffffffff{wide_suffix}");
    let wide_umax = format!("0xffffffffffffffffU{wide_suffix}");

    d.set("__SCHAR_MAX__", "0x7f");
    d.set("__SHRT_MAX__", "0x7fff");
    d.set("__INT_MAX__", "0x7fffffff");
    d.set("__LONG_MAX__", if lp64 { "0x7fffffffffffffffL" } else { "0x7fffffffL" });
    d.set("__LONG_LONG_MAX__", "0x7fffffffffffffffLL");
    d.set("__INTMAX_MAX__", &wide_max);
    d.set("__UINTMAX_MAX__", &wide_umax);
    d.set("__SIZE_MAX__", &wide_umax);
    d.set("__PTRDIFF_MAX__", &wide_max);
    d.set("__INTPTR_MAX__", &wide_max);
    d.set("__UINTPTR_MAX__", &wide_umax);
    d.set("__SIG_ATOMIC_MAX__", "0x7fffffff");
    d.set("__SIG_ATOMIC_MIN__", "(-__SIG_ATOMIC_MAX__ - 1)");
    // The widest `_BitInt` this compiler builds, which is narrower than gcc 16's sixty five
    // thousand five hundred and thirty five because a folded constant here is a hundred and
    // twenty eight bits wide. A program that reads this macro to decide what to write gets an
    // answer it can rely on, which is the point of saying a number smaller than gcc's rather
    // than saying gcc's and refusing what it asked for. `MAX_BIT_INT_WIDTH` in `rucc-sema` is
    // the same number and has to be changed with it.
    d.set("__BITINT_MAXWIDTH__", "128");

    let wchar = wchar(target);
    d.set("__WCHAR_TYPE__", wchar.spelling);
    d.set("__WCHAR_MAX__", wchar.max);
    d.set("__WCHAR_MIN__", wchar.min);
    let wint = wint(target);
    d.set("__WINT_TYPE__", wint.spelling);
    d.set("__WINT_MAX__", wint.max);
    d.set("__WINT_MIN__", wint.min);
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
    exact(d, 8, "signed char", "unsigned char", "0x7f", "0xff", "");
    exact(d, 16, "short int", "short unsigned int", "0x7fff", "0xffff", "");
    // No suffix. An `int` needs none, and the `U` on the unsigned side is added by `exact`
    // rather than being part of the width.
    exact(d, 32, "int", "unsigned int", "0x7fffffff", "0xffffffffU", "");
    exact(d, 64, wide, wide_unsigned, &wide_max, &wide_umax, wide_suffix);

    // The fast types. GCC makes the 16 and 32 bit ones `long` on x86-64 glibc and `int`
    // everywhere else, and a header that computes a printf format from the type name notices
    // the difference.
    //
    // musl is the reason this is not simply a question of the architecture. musl defines
    // `int_fast16_t` and `int_fast32_t` as `int32_t` on every target it supports, GCC built
    // for a musl target agrees with it, and GCC built for glibc on the same processor does
    // not. The place it shows is `stdatomic.h`, which GCC ships and writes directly out of
    // these macros: `typedef _Atomic __INT_FAST16_TYPE__ atomic_int_fast16_t;`. Get this wrong
    // and every atomic fast type in the program is the wrong width.
    let fast_is_wide = target.triple.arch == Arch::X86_64 && lp64 && target.triple.env != Env::Musl;
    let fast_middle = if fast_is_wide { wide } else { "int" };
    d.set("__INT_FAST8_TYPE__", "signed char");
    d.set("__UINT_FAST8_TYPE__", "unsigned char");
    d.set("__INT_FAST8_MAX__", "0x7f");
    d.set("__UINT_FAST8_MAX__", "0xff");
    for width in [16, 32] {
        let unsigned = if fast_middle == "int" { "unsigned int" } else { wide_unsigned };
        let max = if fast_middle == "int" { "0x7fffffff" } else { wide_max.as_str() };
        let umax = if fast_middle == "int" { "0xffffffffU" } else { wide_umax.as_str() };
        d.set(&format!("__INT_FAST{width}_TYPE__"), fast_middle);
        d.set(&format!("__UINT_FAST{width}_TYPE__"), unsigned);
        d.set(&format!("__INT_FAST{width}_MAX__"), max);
        d.set(&format!("__UINT_FAST{width}_MAX__"), umax);
    }
    d.set("__INT_FAST64_TYPE__", wide);
    d.set("__UINT_FAST64_TYPE__", wide_unsigned);
    d.set("__INT_FAST64_MAX__", &wide_max);
    d.set("__UINT_FAST64_MAX__", &wide_umax);

    widths(d, target, &wchar, &wint, if fast_is_wide { 64 } else { 32 });
}

/// The widths, which C23's `limits.h` and `stdint.h` are written out of.
///
/// Twenty macros and not a few more: there is no `__INT8_WIDTH__`, because the width of an
/// exact width type is in its name and gcc does not define one, and there is no unsigned member
/// of any of these pairs, because a signed type and its unsigned counterpart have the same
/// width and `UINTMAX_WIDTH` is written `__INTMAX_WIDTH__` in every header that needs it.
///
/// Each of these says how many value bits and sign bits the type has, which is not the same as
/// how many bits it occupies. They agree for every type on every target here, and the day one of
/// them does not, this is the family that has to say the smaller number.
fn widths(d: &mut Defs, target: &TargetInfo, wchar: &Wchar, wint: &Wint, fast_middle: u32) {
    let pointer = target.pointer_width;
    d.set("__SCHAR_WIDTH__", "8");
    d.set("__SHRT_WIDTH__", "16");
    d.set("__INT_WIDTH__", "32");
    d.set("__LONG_WIDTH__", &target.long_width.to_string());
    d.set("__LONG_LONG_WIDTH__", "64");
    d.set("__INTMAX_WIDTH__", "64");
    d.set("__INTPTR_WIDTH__", &pointer.to_string());
    d.set("__PTRDIFF_WIDTH__", &pointer.to_string());
    d.set("__SIZE_WIDTH__", &pointer.to_string());
    d.set("__SIG_ATOMIC_WIDTH__", "32");
    d.set("__WCHAR_WIDTH__", &(wchar.size * 8).to_string());
    d.set("__WINT_WIDTH__", &wint.width.to_string());
    for width in [8, 16, 32, 64] {
        d.set(&format!("__INT_LEAST{width}_WIDTH__"), &width.to_string());
    }
    d.set("__INT_FAST8_WIDTH__", "8");
    d.set("__INT_FAST16_WIDTH__", &fast_middle.to_string());
    d.set("__INT_FAST32_WIDTH__", &fast_middle.to_string());
    d.set("__INT_FAST64_WIDTH__", "64");
}

/// One width of the exact and least families, which are the same types.
fn exact(
    d: &mut Defs,
    width: u32,
    signed: &str,
    unsigned: &str,
    max: &str,
    umax: &str,
    // The suffix the width needs and nothing more, so `""`, `"L"` or `"LL"`. The `U` that
    // makes a constant unsigned is added below and is not part of this, because a caller that
    // wrote it here would produce `UU` on the unsigned macro and a stray `U` on the signed one.
    width_suffix: &str,
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
    //
    // The `U` goes on only where the type is still unsigned after promotion. `uint8_t` and
    // `uint16_t` are narrower than `int`, so an integer promotion turns them into a signed
    // `int` and `UINT8_C(1)` has that type in gcc and in the standard's own words. Writing
    // `1U` there is not a harmless extra: `UINT8_C(1) - 2` comes out as four billion odd
    // instead of minus one, and a `_Generic` on it picks the unsigned arm. Every target this
    // compiler has makes `int` thirty two bits, which is what makes the width enough to decide.
    let unsigned_after_promotion = width >= 32;
    let u = if unsigned_after_promotion { "U" } else { "" };
    if width_suffix.is_empty() && u.is_empty() {
        d.set(&format!("__INT{width}_C(c)"), "c");
        d.set(&format!("__UINT{width}_C(c)"), "c");
    } else if width_suffix.is_empty() {
        d.set(&format!("__INT{width}_C(c)"), "c");
        d.set(&format!("__UINT{width}_C(c)"), &format!("c ## {u}"));
    } else {
        d.set(&format!("__INT{width}_C(c)"), &format!("c ## {width_suffix}"));
        d.set(&format!("__UINT{width}_C(c)"), &format!("c ## {u}{width_suffix}"));
    }
}

/// What a header needs to know about one floating format, as the text the macros expand to.
///
/// The four values are written to the digit gcc writes them to rather than rounded to something
/// tidier, because a header carrying its own copy of a limit compares the two spellings and a
/// difference in the last place is a difference.
struct Characteristics {
    mant_dig: &'static str,
    dig: &'static str,
    min_exp: &'static str,
    min_10_exp: &'static str,
    max_exp: &'static str,
    max_10_exp: &'static str,
    decimal_dig: &'static str,
    max: &'static str,
    min: &'static str,
    epsilon: &'static str,
    denorm_min: &'static str,
    /// Whether the format is one IEC 60559 describes, which every one of them is but the brain
    /// float, whose significand is a `float`'s with sixteen bits cut off the end of it.
    is_iec_60559: &'static str,
}

/// IEEE binary16, which is `_Float16`.
const HALF: Characteristics = Characteristics {
    mant_dig: "11",
    dig: "3",
    min_exp: "(-13)",
    min_10_exp: "(-4)",
    max_exp: "16",
    max_10_exp: "4",
    decimal_dig: "5",
    max: "6.55040000000000000000000000000000000e+4",
    min: "6.10351562500000000000000000000000000e-5",
    epsilon: "9.76562500000000000000000000000000000e-4",
    denorm_min: "5.96046447753906250000000000000000000e-8",
    is_iec_60559: "1",
};

/// The brain float, which nothing here names yet and which every format table has a row for.
const BFLOAT16: Characteristics = Characteristics {
    mant_dig: "8",
    dig: "2",
    min_exp: "(-125)",
    min_10_exp: "(-37)",
    max_exp: "128",
    max_10_exp: "38",
    decimal_dig: "4",
    max: "3.38953138925153547590470800371487867e+38",
    min: "1.17549435082228750796873653722224568e-38",
    epsilon: "7.81250000000000000000000000000000000e-3",
    denorm_min: "9.18354961579912115600575419704879436e-41",
    is_iec_60559: "0",
};

/// IEEE binary32, which is `float` and `_Float32`.
const SINGLE: Characteristics = Characteristics {
    mant_dig: "24",
    dig: "6",
    min_exp: "(-125)",
    min_10_exp: "(-37)",
    max_exp: "128",
    max_10_exp: "38",
    decimal_dig: "9",
    max: "3.40282346638528859811704183484516925e+38",
    min: "1.17549435082228750796873653722224568e-38",
    epsilon: "1.19209289550781250000000000000000000e-7",
    denorm_min: "1.40129846432481707092372958328991613e-45",
    is_iec_60559: "1",
};

/// IEEE binary64, which is `double`, `_Float64`, `_Float32x` and `long double` on Apple and
/// on Windows.
const DOUBLE: Characteristics = Characteristics {
    mant_dig: "53",
    dig: "15",
    min_exp: "(-1021)",
    min_10_exp: "(-307)",
    max_exp: "1024",
    max_10_exp: "308",
    decimal_dig: "17",
    max: "1.79769313486231570814527423731704357e+308",
    min: "2.22507385850720138309023271733240406e-308",
    epsilon: "2.22044604925031308084726333618164062e-16",
    denorm_min: "4.94065645841246544176568792868221372e-324",
    is_iec_60559: "1",
};

/// The x87 eighty bit format, which on x86-64 is both `long double` and `_Float64x`.
const X87: Characteristics = Characteristics {
    mant_dig: "64",
    dig: "18",
    min_exp: "(-16381)",
    min_10_exp: "(-4931)",
    max_exp: "16384",
    max_10_exp: "4932",
    decimal_dig: "21",
    max: "1.18973149535723176502126385303097021e+4932",
    min: "3.36210314311209350626267781732175260e-4932",
    epsilon: "1.08420217248550443400745280086994171e-19",
    denorm_min: "3.64519953188247460252840593361941982e-4951",
    is_iec_60559: "1",
};

/// IEEE binary128, which is `_Float128`, `_Float64x` off x86 and `long double` on AArch64 and
/// RISC-V Linux.
const QUAD: Characteristics = Characteristics {
    mant_dig: "113",
    dig: "33",
    min_exp: "(-16381)",
    min_10_exp: "(-4931)",
    max_exp: "16384",
    max_10_exp: "4932",
    decimal_dig: "36",
    max: "1.18973149535723176508575932662800702e+4932",
    min: "3.36210314311209350626267781732175260e-4932",
    epsilon: "1.92592994438723585305597794258492732e-34",
    denorm_min: "6.47517511943802511092443895822764655e-4966",
    is_iec_60559: "1",
};

/// The row of the table a format has, so that a type the target chooses the format of can look
/// its own limits up rather than have them written out again per architecture.
const fn characteristics(format: Format) -> &'static Characteristics {
    match format {
        Format::Half => &HALF,
        Format::BFloat16 => &BFLOAT16,
        Format::Single => &SINGLE,
        Format::Double => &DOUBLE,
        Format::X87Extended => &X87,
        Format::Quad => &QUAD,
    }
}

/// The `float.h` characteristics.
///
/// Nine families of them, which is `float`, `double` and `long double` and the six C23 named
/// them after. Only two of the nine depend on the target, and they are the two whose format is
/// a target property: `long double`, which is x87 on x86-64 Linux, quad on AArch64 and RISC-V
/// Linux and a `double` on Apple and on Windows, and `_Float64x`, which is the widest format the
/// processor has and so does not follow `long double` down on the targets that shrink it.
///
/// `__FLT128X_*__` is deliberately missing. `_Float128x` is a type no target gcc supports has,
/// so gcc defines nothing for it and neither does this.
fn floats(d: &mut Defs, target: &TargetInfo) {
    d.set("__FLT_RADIX__", "2");
    // Every operation is done in the type of its operands, which is what SSE2 and the AArch64
    // and RISC-V floating units all do. The other two names are the same answer asked under the
    // rules of C99 and of TS 18661-3, which are the same rules for a target with no excess
    // precision to have, and glibc's `<math.h>` reads the last of the three.
    d.set("__FLT_EVAL_METHOD__", "0");
    d.set("__FLT_EVAL_METHOD_C99__", "0");
    d.set("__FLT_EVAL_METHOD_TS_18661_3__", "0");

    family(d, "FLT", &SINGLE, |value| format!("{value}F"));
    // gcc writes the `double` values as `long double` constants cast back down, which is exact
    // in every format `long double` has and is the one family whose values are not a suffix.
    family(d, "DBL", &DOUBLE, |value| format!("((double){value}L)"));
    family(d, "LDBL", characteristics(target.long_double_format), |value| format!("{value}L"));

    family(d, "FLT16", &HALF, |value| format!("{value}F16"));
    family(d, "FLT32", &SINGLE, |value| format!("{value}F32"));
    family(d, "FLT64", &DOUBLE, |value| format!("{value}F64"));
    family(d, "FLT128", &QUAD, |value| format!("{value}F128"));
    family(d, "FLT32X", &DOUBLE, |value| format!("{value}F32x"));
    family(d, "FLT64X", characteristics(target.float64x_format), |value| format!("{value}F64x"));

    // The number itself rather than the name of the other macro. The value is the same either
    // way, since `long double` is the widest format here, but the two are not the same thing to
    // read: `-dM` prints what the macro is, and a program that undefines `__LDBL_DECIMAL_DIG__`
    // takes this one with it. gcc writes the number.
    d.set("__DECIMAL_DIG__", characteristics(target.long_double_format).decimal_dig);
}

/// One family of `float.h` macros, named `__{prefix}_*__`.
///
/// `write` turns a value into the constant its macro expands to, which is a suffix for every
/// family but `double`. `NORM_MAX` is `MAX` for all six formats, since the two differ only
/// where a format holds values above its largest normal one and none of these do.
fn family(d: &mut Defs, prefix: &str, c: &Characteristics, write: impl Fn(&str) -> String) {
    d.set(&format!("__{prefix}_MANT_DIG__"), c.mant_dig);
    d.set(&format!("__{prefix}_DIG__"), c.dig);
    d.set(&format!("__{prefix}_MIN_EXP__"), c.min_exp);
    d.set(&format!("__{prefix}_MIN_10_EXP__"), c.min_10_exp);
    d.set(&format!("__{prefix}_MAX_EXP__"), c.max_exp);
    d.set(&format!("__{prefix}_MAX_10_EXP__"), c.max_10_exp);
    d.set(&format!("__{prefix}_DECIMAL_DIG__"), c.decimal_dig);
    d.set(&format!("__{prefix}_MAX__"), &write(c.max));
    d.set(&format!("__{prefix}_NORM_MAX__"), &write(c.max));
    d.set(&format!("__{prefix}_MIN__"), &write(c.min));
    d.set(&format!("__{prefix}_EPSILON__"), &write(c.epsilon));
    d.set(&format!("__{prefix}_DENORM_MIN__"), &write(c.denorm_min));
    d.set(&format!("__{prefix}_IS_IEC_60559__"), c.is_iec_60559);
    d.set(&format!("__{prefix}_HAS_DENORM__"), "1");
    d.set(&format!("__{prefix}_HAS_INFINITY__"), "1");
    d.set(&format!("__{prefix}_HAS_QUIET_NAN__"), "1");
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
    fn apple_spells_the_architecture_its_own_way_and_its_headers_only_know_that_spelling() {
        // sys/cdefs.h reaches #error "Unsupported architecture" without these, which is the
        // first line of the first header of every program on the platform.
        let darwin = set_for("aarch64-apple-darwin");
        assert!(has(&darwin, "#define __arm64__ 1"));
        assert!(has(&darwin, "#define __arm64 1"));
        assert!(has(&darwin, "#define __aarch64__ 1"), "the portable spelling stays too");
        let linux = set_for("aarch64-unknown-linux-gnu");
        assert!(!has(&linux, "#define __arm64__ 1"), "Apple's spelling is Apple's alone");
        assert!(!has(&set_for("x86_64-apple-darwin"), "#define __arm64__ 1"));
    }

    #[test]
    fn every_limit_is_spelled_in_hexadecimal_the_way_gcc_spells_it() {
        // The value was never in question and the spelling is, because these macros reach a
        // program's text. glibc's `limits.h` writes `#define INT_MAX __INT_MAX__`, openssl
        // writes `((unsigned int)INT_MAX + 1)`, and `-E` over that header printed a decimal
        // number where gcc printed a hexadecimal one. The type is the same either way here,
        // which is why the suffixes are unchanged: `0x7fffffff` and `2147483647` are both
        // `int`, and `0xffffffffffffffffUL` and its decimal twin are both `unsigned long`.
        let linux = set_for("x86_64-unknown-linux-gnu");
        for line in [
            "#define __SCHAR_MAX__ 0x7f",
            "#define __SHRT_MAX__ 0x7fff",
            "#define __INT_MAX__ 0x7fffffff",
            "#define __LONG_MAX__ 0x7fffffffffffffffL",
            "#define __LONG_LONG_MAX__ 0x7fffffffffffffffLL",
            "#define __INTMAX_MAX__ 0x7fffffffffffffffL",
            "#define __UINTMAX_MAX__ 0xffffffffffffffffUL",
            "#define __SIZE_MAX__ 0xffffffffffffffffUL",
            "#define __PTRDIFF_MAX__ 0x7fffffffffffffffL",
            "#define __SIG_ATOMIC_MAX__ 0x7fffffff",
            "#define __INT8_MAX__ 0x7f",
            "#define __UINT8_MAX__ 0xff",
            "#define __INT16_MAX__ 0x7fff",
            "#define __UINT16_MAX__ 0xffff",
            "#define __INT32_MAX__ 0x7fffffff",
            "#define __UINT32_MAX__ 0xffffffffU",
            "#define __INT64_MAX__ 0x7fffffffffffffffL",
            "#define __UINT64_MAX__ 0xffffffffffffffffUL",
            "#define __INT_FAST8_MAX__ 0x7f",
            "#define __UINT_FAST8_MAX__ 0xff",
        ] {
            assert!(has(&linux, line), "{line}");
        }
        // Windows, where `long` is thirty two bits, so the wide suffix moves and the narrow
        // `long` limit is not the same number.
        let windows = set_for("x86_64-pc-windows-msvc");
        assert!(has(&windows, "#define __LONG_MAX__ 0x7fffffffL"));
        assert!(has(&windows, "#define __INTMAX_MAX__ 0x7fffffffffffffffLL"));
        assert!(has(&windows, "#define __UINTMAX_MAX__ 0xffffffffffffffffULL"));
    }

    #[test]
    fn wint_t_does_not_follow_wchar_t() {
        // Apple makes it signed so that WEOF is negative the way EOF is. Linux does not.
        let darwin = set_for("aarch64-apple-darwin");
        assert!(has(&darwin, "#define __WINT_TYPE__ int"));
        assert!(has(&darwin, "#define __WINT_MAX__ 0x7fffffff"));
        assert!(has(&darwin, "#define __WCHAR_TYPE__ int"));
        let linux = set_for("aarch64-unknown-linux-gnu");
        assert!(has(&linux, "#define __WINT_TYPE__ unsigned int"));
        assert!(has(&linux, "#define __WINT_MAX__ 0xffffffffU"));
        assert!(has(&linux, "#define __WCHAR_TYPE__ unsigned int"), "and wchar_t is its own");
        assert!(has(
            &set_for("x86_64-pc-windows-msvc"),
            "#define __WINT_TYPE__ short unsigned int"
        ));
    }

    #[test]
    fn the_widths_say_what_the_type_holds_and_follow_the_target_that_changes_it() {
        // Twenty of them, which is gcc's set: no exact width member, since the width of an
        // `int32_t` is in its name, and no unsigned member, since a header that wants
        // `UINTMAX_WIDTH` writes `__INTMAX_WIDTH__`.
        let linux = set_for("x86_64-unknown-linux-gnu");
        assert_eq!(linux.lines().filter(|line| line.contains("_WIDTH__")).count(), 20);
        assert!(has(&linux, "#define __LONG_WIDTH__ 64"));
        assert!(has(&linux, "#define __SIZE_WIDTH__ 64"));
        assert!(has(&linux, "#define __WCHAR_WIDTH__ 32"));
        assert!(has(&linux, "#define __INT_LEAST16_WIDTH__ 16"));
        // x86-64 glibc is where `int_fast16_t` is a `long`, and the width has to say so or a
        // program that switches on it picks the wrong branch.
        assert!(has(&linux, "#define __INT_FAST16_WIDTH__ 64"));
        assert!(has(&set_for("x86_64-unknown-linux-musl"), "#define __INT_FAST16_WIDTH__ 32"));
        // Windows has a thirty two bit `long` and a sixteen bit `wint_t`, and the pointer
        // sized types stay sixty four bits wide whatever `long` does.
        let windows = set_for("x86_64-pc-windows-msvc");
        assert!(has(&windows, "#define __LONG_WIDTH__ 32"));
        assert!(has(&windows, "#define __WINT_WIDTH__ 16"));
        assert!(has(&windows, "#define __SIZE_WIDTH__ 64"));
        assert!(has(&windows, "#define __INTMAX_WIDTH__ 64"));
    }

    #[test]
    fn a_constant_maker_gets_the_suffix_its_width_needs_and_no_other() {
        // Found by diffing `-dM` against the system compiler. The 32 bit row was passing `U`
        // as its width suffix, which put a `U` on the signed macro and two on the unsigned
        // one, and `UINT32_C(1)` expanded to `1UU`, which is not a token.
        let linux = set_for("x86_64-unknown-linux-gnu");
        assert!(has(&linux, "#define __INT32_C(c) c"));
        assert!(has(&linux, "#define __UINT32_C(c) c ## U"));
        assert!(has(&linux, "#define __INT16_C(c) c"));
        // No `U` on the two narrow ones, because `uint8_t` and `uint16_t` promote to a
        // signed `int` and the constant has that type. gcc leaves it off for the same reason.
        assert!(has(&linux, "#define __UINT16_C(c) c"));
        assert!(has(&linux, "#define __UINT8_C(c) c"));
        // The wide ones do take a suffix, and the `U` goes in front of it.
        assert!(has(&linux, "#define __INT64_C(c) c ## L"));
        assert!(has(&linux, "#define __UINT64_C(c) c ## UL"));
        // Windows has a thirty two bit `long`, so its sixty four bit constants are `long long`.
        let windows = set_for("x86_64-pc-windows-msvc");
        assert!(has(&windows, "#define __INT64_C(c) c ## LL"));
        assert!(has(&windows, "#define __UINT64_C(c) c ## ULL"));
    }

    #[test]
    fn the_symbol_prefix_is_defined_everywhere_including_where_it_is_empty() {
        // Empty is not the same as absent, because glibc stringifies it. Leaving it undefined
        // turns `__asm__ (__ASMNAME ("__xpg_strerror_r"))` into an asm name of
        // "__USER_LABEL_PREFIX__" "__xpg_strerror_r", which renames the function instead of
        // failing, and that is a bug found at link time or later.
        for triple in
            ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu", "x86_64-pc-windows-msvc"]
        {
            assert!(has(&set_for(triple), "#define __USER_LABEL_PREFIX__ "), "{triple}");
        }
        // Mach-O keeps the underscore that ELF dropped.
        assert!(has(&set_for("aarch64-apple-darwin"), "#define __USER_LABEL_PREFIX__ _"));
    }

    #[test]
    fn the_memory_orders_are_there_even_without_atomics() {
        // musl's stdatomic.h writes `memory_order_relaxed = __ATOMIC_RELAXED` with no test
        // around it, so these are not a promise about `_Atomic`, they are the numbering the
        // builtins take, and a compiler without them prints an enumerator whose value is an
        // identifier.
        let linux = set_for("x86_64-unknown-linux-gnu");
        assert!(has(&linux, "#define __ATOMIC_RELAXED 0"));
        assert!(has(&linux, "#define __ATOMIC_SEQ_CST 5"));
        assert!(has(&linux, "#define __STDC_NO_ATOMICS__ 1"), "and we still have no _Atomic");
        assert!(has(&linux, "#define __GCC_ATOMIC_INT_LOCK_FREE 2"));
        assert!(has(&linux, "#define __GCC_ATOMIC_LLONG_LOCK_FREE 2"));
        assert!(has(&set_for("x86_64-pc-windows-msvc"), "#define __GCC_ATOMIC_LLONG_LOCK_FREE 2"));
    }

    #[test]
    fn long_double_is_three_types_and_the_macros_say_which() {
        assert!(has(&set_for("x86_64-unknown-linux-gnu"), "#define __LDBL_MANT_DIG__ 64"));
        assert!(has(&set_for("aarch64-unknown-linux-gnu"), "#define __LDBL_MANT_DIG__ 113"));
        assert!(has(&set_for("aarch64-apple-darwin"), "#define __LDBL_MANT_DIG__ 53"));
    }

    #[test]
    fn the_extended_floating_types_have_the_limits_their_formats_have() {
        // Every one of these but `_Float64x` is the same format on every target, which is the
        // point of the interchange types, so the limits are the same everywhere too.
        let linux = set_for("x86_64-unknown-linux-gnu");
        assert!(has(&linux, "#define __FLT16_MANT_DIG__ 11"));
        assert!(has(&linux, "#define __FLT32_MANT_DIG__ 24"));
        assert!(has(&linux, "#define __FLT64_MANT_DIG__ 53"));
        assert!(has(&linux, "#define __FLT128_MANT_DIG__ 113"));
        assert!(has(&linux, "#define __FLT32X_MANT_DIG__ 53"));
        // Each family writes its values with its own suffix, so a header that assigns one to an
        // object of the type gets the type back rather than a conversion.
        assert!(has(&linux, "#define __FLT16_MAX__ 6.55040000000000000000000000000000000e+4F16"));
        assert!(has(
            &linux,
            "#define __FLT32X_MIN__ 2.22507385850720138309023271733240406e-308F32x"
        ));
        // `_Float128x` is a type no target has, so gcc defines nothing for it and neither
        // does this.
        assert!(!linux.contains("__FLT128X_"));
    }

    #[test]
    fn float64x_keeps_the_width_that_long_double_loses_on_apple() {
        // The two are the same eighty bit x87 format on x86-64 and part company everywhere
        // else, because `_Float64x` follows the processor and `long double` follows the ABI.
        let linux = set_for("x86_64-unknown-linux-gnu");
        assert!(has(&linux, "#define __FLT64X_MANT_DIG__ 64"));
        assert!(has(&linux, "#define __LDBL_MANT_DIG__ 64"));
        let mac = set_for("aarch64-apple-darwin");
        assert!(has(&mac, "#define __FLT64X_MANT_DIG__ 113"));
        assert!(has(&mac, "#define __LDBL_MANT_DIG__ 53"));
        let windows = set_for("x86_64-pc-windows-msvc");
        assert!(has(&windows, "#define __FLT64X_MANT_DIG__ 64"));
        assert!(has(&windows, "#define __LDBL_MANT_DIG__ 53"));
    }

    #[test]
    fn the_largest_value_of_a_binary_format_is_also_its_largest_normal_one() {
        // `NORM_MAX` is only ever smaller than `MAX` for a format that holds values above its
        // largest normal one, and none of the six here does.
        let linux = set_for("x86_64-unknown-linux-gnu");
        for prefix in ["FLT", "DBL", "LDBL", "FLT16", "FLT32", "FLT64", "FLT128", "FLT32X"] {
            let value = |suffix: &str| {
                let name = format!("#define __{prefix}_{suffix}__ ");
                let line = linux
                    .lines()
                    .find(|line| line.starts_with(&name))
                    .unwrap_or_else(|| panic!("__{prefix}_{suffix}__ is defined"));
                line[name.len()..].to_owned()
            };
            assert_eq!(value("MAX"), value("NORM_MAX"), "__{prefix}_NORM_MAX__");
        }
    }

    #[test]
    fn the_widest_bit_int_is_said_in_every_dialect() {
        // gcc defines it under `-std=c17` as well as `-std=c23`, and a header that reaches for
        // `_BitInt` tests the macro rather than the version, so an absent one reads as a
        // compiler without the type at all.
        let mut opts = Predef::new();
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse().unwrap());
        assert!(has(&built_in(&target, &opts), "#define __BITINT_MAXWIDTH__ 128"));
        opts.std = Std::C17;
        assert!(has(&built_in(&target, &opts), "#define __BITINT_MAXWIDTH__ 128"));
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

    /// The conditional feature macros are claims not to have something, and a claim that is
    /// not true changes what a header declares rather than turning anything off.
    #[test]
    fn the_only_things_claimed_missing_are_the_ones_that_are_missing() {
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse().unwrap());
        let opts = Predef::new();
        let set = built_in(&target, &opts);
        assert!(has(&set, "#define __STDC_NO_ATOMICS__ 1"), "there is no stdatomic.h to include");
        assert!(has(&set, "#define __STDC_NO_THREADS__ 1"), "nor a threads.h");
        assert!(has(&set, "#define __STDC_NO_COMPLEX__ 1"), "the arithmetic is not lowered");
        assert!(!set.contains("__STDC_NO_VLA__"), "variable length arrays work");
    }

    /// gcc's own `stdatomic.h` declares `atomic_char8_t` under `#ifdef __CHAR8_TYPE__`, so a
    /// compiler that defines it in C17 declares a type gcc does not and one that never defines
    /// it is missing one in C23. Both were caught by preprocessing that header both ways.
    #[test]
    fn the_type_behind_char8_t_is_defined_in_c23_and_in_no_dialect_before_it() {
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse().unwrap());
        let mut opts = Predef::new();
        assert!(has(&built_in(&target, &opts), "#define __CHAR8_TYPE__ unsigned char"));

        for older in [Std::C17, Std::C11, Std::C99, Std::C89] {
            opts.std = older;
            assert!(!built_in(&target, &opts).contains("__CHAR8_TYPE__"), "{older:?}");
        }
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
        assert!(has(&built_in(&target, &opts), "#define __GNUC__ 7"));
        opts.gnuc = GnucVersion { major: 15, minor: 1, patch: 0 };
        assert!(has(&built_in(&target, &opts), "#define __GNUC__ 15"));
        assert!(has(&built_in(&target, &opts), "#define __GNUC_MINOR__ 1"));
    }

    #[test]
    fn musl_and_glibc_disagree_about_the_fast_types_on_the_same_processor() {
        // The same x86-64 machine, two libcs, two answers. GCC built for glibc says `long int`
        // and GCC built for musl says `int`, because musl defines `int_fast16_t` as `int32_t`
        // everywhere. It shows in `stdatomic.h`, which GCC writes out of these macros, so
        // getting it wrong makes every atomic fast type the wrong width.
        let gnu = set_for("x86_64-unknown-linux-gnu");
        let musl = set_for("x86_64-unknown-linux-musl");
        assert!(has(&gnu, "#define __INT_FAST16_TYPE__ long int"));
        assert!(has(&gnu, "#define __INT_FAST32_TYPE__ long int"));
        assert!(has(&gnu, "#define __UINT_FAST16_TYPE__ long unsigned int"));
        assert!(has(&musl, "#define __INT_FAST16_TYPE__ int"));
        assert!(has(&musl, "#define __INT_FAST32_TYPE__ int"));
        assert!(has(&musl, "#define __UINT_FAST16_TYPE__ unsigned int"));
        // The limits have to move with the types or a header that checks them stops agreeing
        // with the header that uses them.
        assert!(has(&gnu, "#define __INT_FAST16_MAX__ 0x7fffffffffffffffL"));
        assert!(has(&musl, "#define __INT_FAST16_MAX__ 0x7fffffff"));
        assert!(has(&musl, "#define __UINT_FAST16_MAX__ 0xffffffffU"));
    }

    #[test]
    fn the_libc_only_moves_the_two_fast_types_it_is_allowed_to_move() {
        // 8 and 64 are the same on both, and so is everything outside the fast family. A libc
        // is not a processor and this is the whole of what it is permitted to change here.
        let gnu = set_for("x86_64-unknown-linux-gnu");
        let musl = set_for("x86_64-unknown-linux-musl");
        for line in [
            "#define __INT_FAST8_TYPE__ signed char",
            "#define __INT_FAST64_TYPE__ long int",
            "#define __INT64_TYPE__ long int",
            "#define __SIZE_TYPE__ long unsigned int",
            "#define __SIZEOF_LONG__ 8",
            "#define __LP64__ 1",
        ] {
            assert!(has(&gnu, line), "glibc lost {line}");
            assert!(has(&musl, line), "musl lost {line}");
        }
    }

    #[test]
    fn a_non_x86_target_has_int_sized_fast_types_whatever_the_libc() {
        // The `long` answer was always specific to x86-64. aarch64 glibc says `int` too, so
        // adding the libc axis must not have turned into a second way to say x86-64.
        let arm_gnu = set_for("aarch64-unknown-linux-gnu");
        let arm_musl = set_for("aarch64-unknown-linux-musl");
        assert!(has(&arm_gnu, "#define __INT_FAST16_TYPE__ int"));
        assert!(has(&arm_musl, "#define __INT_FAST16_TYPE__ int"));
    }
}
