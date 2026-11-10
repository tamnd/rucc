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
use rucc_session::{
    Control, GnucVersion, Math, MscVersion, OptLevel, Options, Pic, Protector, Std,
};
use rucc_target::{
    Arch, BranchProtection, CodeModel, Env, Feature, Isa, ObjectFormat, Os, Preview, Processor,
    SignReturn, TargetInfo, Triple, wasm,
};
use rucc_tuple::{self as tuple};

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
    /// Whether the unit is in the kencc dialect, which is `-fkencc`. kencc defines no macro, so
    /// the set is empty and only the command line defines macros.
    pub kencc: bool,
    /// Whether a kencc unit goes through the ANSI C preprocessor, which is `-p` or
    /// `-fno-kencc-preprocessor`. Of the macros that `/bin/cpp` defines, `__DATE__` and
    /// `__TIME__` are then in the set. `__LINE__` and `__FILE__` are always defined.
    pub kencc_cpp: bool,
    /// Whether the GNU extensions are on, which is `-std=gnu23` rather than `-std=c23`. It
    /// decides `__STRICT_ANSI__` and the unarmoured `linux` and `unix` macros.
    pub gnu_extensions: bool,
    /// Whether the unit is under GNU's reading of `inline`, which is `-fgnu89-inline`. It decides
    /// which of `__GNUC_GNU_INLINE__` and `__GNUC_STDC_INLINE__` is defined, and the C89 dialects
    /// are under that reading whatever it says.
    pub gnu89_inline: bool,
    /// The GCC release claimed.
    pub gnuc: GnucVersion,
    /// Whether `-fgnuc-version=` was written, which is the only way an MSVC row claims one.
    pub gnuc_given: bool,
    /// The MSVC release an MSVC row claims, which is `_MSC_VER` and `_MSC_FULL_VER`.
    pub msc: MscVersion,
    /// Whether an MSVC row's C runtime is the DLL one, which defines `_DLL` beside `_MT`.
    pub ms_dll_runtime: bool,
    /// `-fms-extensions` and `-fno-ms-extensions`, with the target deciding when neither was
    /// written. On an MSVC row it decides `_MSC_EXTENSIONS`.
    pub ms_extensions: Option<bool>,
    /// Decides `__OPTIMIZE__`, `__OPTIMIZE_SIZE__` and `__NO_INLINE__`.
    pub opt_level: OptLevel,
    /// Whether there is a standard library, which is `-ffreestanding` turned around.
    pub hosted: bool,
    /// Which link the output is for, from `-fPIC` and `-fPIE`. It decides `__PIE__`, since
    /// `__PIC__` is defined either way and says only that there are no absolute addresses.
    pub pic: Pic,
    /// `-mcmodel=`, which decides whether `__code_model_small__` or `__code_model_kernel__` is
    /// defined on x86-64.
    pub code_model: CodeModel,
    /// `__DATE__` and `__TIME__`.
    pub timestamp: Timestamp,
    /// The glibc release the headers are, as the minor number alone, when they are ours.
    ///
    /// `__GLIBC_MINOR__` and nothing else: `__GLIBC__` is 2 in the tree itself, which is how Zig's
    /// own patched `features.h` has it, and a version the compiler supplied and a version the
    /// header supplied would be two answers to one question.
    pub glibc_minor: Option<u32>,
    /// Whether an operation may raise an exception the program looks at, from `-ftrapping-math`.
    pub trapping_math: bool,
    /// The rest of the `-ffast-math` family. Each licence has a macro of its own and the family
    /// together decides `__FAST_MATH__` and whether the arithmetic is still IEC 60559's.
    pub math: Math,
    /// Whether an exception may unwind through this unit, from `-fexceptions`.
    pub exceptions: bool,
    /// Whether the functions of this unit get unwind tables, which is
    /// [`Options::unwinds`](rucc_session::Options::unwinds).
    pub unwinds: bool,
    /// `-D` in command line order. `FOO` means `FOO=1`, as GCC has it.
    pub defines: Vec<String>,
    /// `-U` in command line order, applied after the defines. No define comes after an undefine
    /// of its name on the command line, because the driver takes the undefine back.
    pub undefines: Vec<String>,
    /// The instruction set extensions the unit is built for, which decides `__SSE4_2__` and the
    /// rest of that family on x86-64 and nothing anywhere else.
    pub isa: Isa,
    /// The WebAssembly features the unit is built for, which decide the `__wasm_<feature>__`
    /// macros on a wasm target and nothing anywhere else.
    pub wasm: wasm::Features,
    /// Whether a value may be kept in a vector register, which `-mgeneral-regs-only` turns off.
    /// On AArch64 it decides `__ARM_FP`, `__ARM_NEON` and the fused multiply add macros, which gcc
    /// leaves out under that flag. On x86-64 the extensions say the same thing, through `__SSE__`.
    pub vector: bool,
    /// Whether a float argument travels in the integer registers, which is RISC-V's `-mabi=lp64`
    /// and decides `__riscv_float_abi_soft`.
    pub soft_float: bool,
    /// The RISC-V `-march=` as written, or `None` for `rv64gc`. It decides the extension macros.
    pub riscv_arch: Option<String>,
    /// Whether there is an x87 unit, which `-msoft-float` and `-mno-80387` take away on x86. gcc
    /// says `_SOFT_FLOAT` without one.
    pub x87: bool,
    /// Whether `-mstrict-align` was given, which takes `__ARM_FEATURE_UNALIGNED` away on AArch64
    /// as it does in gcc.
    pub strict_align: bool,
    /// Whether `-march=` or `-mtune=` named a processor to tune for, which takes away the default
    /// tuning a BSD gcc was configured with.
    pub tuned: bool,
    /// The 32 bit x86 processor the unit is built for, see
    /// [`Options::processor`](rucc_session::Options::processor). `None` is the Pentium 4.
    pub processor: Option<&'static Processor>,
    /// The macros that name the processor the unit is tuned for on 32 bit x86, see
    /// [`Options::tune`](rucc_session::Options::tune).
    pub tune: &'static [&'static str],
    /// Whether `float` and `double` arithmetic is done on the x87 stack, which `-mfpmath=387`
    /// asks for on 32 bit x86. See [`Options::x87_floats`](rucc_session::Options::x87_floats).
    pub x87_floats: bool,
    /// What `-mbranch-protection=` asked for, which decides `__ARM_FEATURE_BTI_DEFAULT` and
    /// `__ARM_FEATURE_PAC_DEFAULT` on AArch64. The kernel's assembly reads both to write the same
    /// note the compiler writes for C.
    pub branch_protection: BranchProtection,
    /// Which functions get a stack protector, which decides `__SSP__` and the three macros like it.
    /// glibc reads them to decide how to build its own code that must not have a canary.
    pub protector: Protector,
    /// Whether `-fsanitize=kernel-address` is on, which decides `__SANITIZE_ADDRESS__`.
    pub address: bool,
    /// Whether any check of `-fsanitize=undefined` is on, which decides what
    /// `__has_feature(undefined_behavior_sanitizer)` answers.
    pub undefined: bool,
    /// Whether `-pedantic-errors` is on, which makes `__has_extension` answer as `__has_feature`
    /// does.
    pub pedantic_errors: bool,
    /// What `-fcf-protection=` asked for, which decides `__CET__` on x86. Assembly reads it to put
    /// `endbr64` at each entry and to write the note that the C code gets from the compiler.
    pub control: Control,
    /// Whether the file is assembly on its way to the assembler, `.S` or `-x assembler-with-cpp`.
    /// It defines `__ASSEMBLER__`, which every header that is also read from assembly tests to
    /// leave its C declarations out, and it takes away the macros that describe the C language
    /// rather than the machine, the way gcc's `-lang-asm` does.
    pub assembler: bool,
    /// Whether `-fbuilding-libgcc` was given, which defines the `__LIBGCC_*__` macros that gcc's
    /// own runtime library reads to learn about the machine. The function `libgcc` gives them.
    pub building_libgcc: bool,
}

impl Predef {
    /// The default dialect, `gnu23`, at `-O0`.
    pub fn new() -> Predef {
        Predef {
            std: Std::default(),
            kencc: false,
            kencc_cpp: false,
            gnu_extensions: true,
            gnu89_inline: false,
            gnuc: GnucVersion::default(),
            gnuc_given: false,
            msc: MscVersion::default(),
            ms_dll_runtime: false,
            ms_extensions: None,
            opt_level: OptLevel::O0,
            hosted: true,
            pic: Pic::Executable,
            code_model: CodeModel::Small,
            timestamp: Timestamp::now(),
            glibc_minor: None,
            trapping_math: true,
            math: Math::default(),
            exceptions: false,
            unwinds: true,
            defines: Vec::new(),
            undefines: Vec::new(),
            isa: Isa::baseline(),
            wasm: wasm::Cpu::default().features(),
            vector: true,
            soft_float: false,
            riscv_arch: None,
            x87: true,
            strict_align: false,
            tuned: false,
            processor: None,
            tune: &[],
            x87_floats: false,
            branch_protection: BranchProtection::default(),
            protector: Protector::None,
            address: false,
            undefined: false,
            pedantic_errors: false,
            control: Control::None,
            assembler: false,
            building_libgcc: false,
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
            kencc: opts.kencc,
            kencc_cpp: opts.kencc && opts.ansi_preprocessor,
            gnu_extensions: opts.gnu_extensions,
            gnu89_inline: opts.gnu89_inline,
            gnuc: opts.gnuc,
            gnuc_given: opts.gnuc_given,
            msc: opts.msc,
            ms_dll_runtime: opts.ms_dll_runtime,
            ms_extensions: opts.ms_extensions,
            opt_level: opts.opt_level,
            hosted: opts.hosted,
            pic: opts.pic,
            code_model: opts.code_model,
            timestamp: Timestamp::now(),
            glibc_minor: opts.glibc_minor,
            trapping_math: opts.trapping_math,
            math: opts.math,
            exceptions: opts.exceptions,
            unwinds: opts.unwinds(),
            defines: opts.defines.clone(),
            undefines: opts.undefines.clone(),
            isa: opts.isa,
            wasm: opts.wasm,
            vector: opts.vector,
            soft_float: opts.soft_float,
            riscv_arch: opts.riscv_arch.clone(),
            x87: opts.x87,
            strict_align: opts.strict_align,
            tuned: opts.tuned,
            processor: opts.processor,
            tune: opts.tune,
            x87_floats: opts.x87_floats,
            branch_protection: opts.branch_protection,
            protector: opts.protector,
            address: opts.kasan.on,
            undefined: opts.sanitize.any(),
            pedantic_errors: opts.named_warnings.has_pedantic_errors(),
            control: opts.control,
            assembler: false,
            building_libgcc: opts.building_libgcc,
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

/// The predefined set of `/bin/cpp` in Plan 9, as the text of a file. It has the two stamps.
pub(crate) fn kencc_cpp(opts: &Predef) -> String {
    let mut d = Defs::new();
    d.set("__DATE__", &format!("\"{}\"", opts.timestamp.date));
    d.set("__TIME__", &format!("\"{}\"", opts.timestamp.time));
    d.text
}

/// The whole predefined set for a target, as the text of a file.
pub(crate) fn built_in(target: &TargetInfo, opts: &Predef) -> String {
    let mut d = Defs::new();
    identity(&mut d, target, opts);
    // `__DATE__` and `__TIME__` are fixed for the whole translation unit, which is what the
    // standard asks for, so they are ordinary object-like macros and the expander needs to
    // know nothing about them.
    d.set("__DATE__", &format!("\"{}\"", opts.timestamp.date));
    d.set("__TIME__", &format!("\"{}\"", opts.timestamp.time));
    dialect(&mut d, target, opts);
    optimization(&mut d, opts);
    platform(&mut d, target, opts);
    hardening(&mut d, target, opts);
    sizes(&mut d, target, opts);
    integers(&mut d, target, opts);
    floats(&mut d, target, opts);
    atomics(&mut d, target, opts);
    if opts.building_libgcc {
        libgcc(&mut d, target);
    }
    d.text
}

/// `-D` and `-U`, as the text of a file.
///
/// Empty when there are none, so that the caller can skip adding a file that would say
/// nothing. GCC reads `-D` and `-U` in command line order, and the driver takes back an earlier
/// flag of the other kind for the same name. So no define comes after an undefine of its name, and
/// the undefines can come last.
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

/// What this compiler says its own version is.
///
/// Read from the manifest at build time rather than written here, because a number written in a
/// second place is a number that goes stale: these five macros said 0.1.0 through sixty seven
/// releases. A program testing `__rucc_major__` for a feature was told the answer for a version
/// nobody has run since, and a build log recording `__VERSION__` recorded the wrong compiler.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// One dotted field of a version, as the digits at the front of it.
///
/// `0.10.68-rc.1` has a patch level of 68 and not of `68-rc`, and a field that is not there at all
/// is zero, which is what a two field version means by its third. Neither shape is one this
/// workspace publishes, and a macro that expands to something no `#if` can read is worse than a
/// macro that is approximately right.
fn field(version: &str, n: usize) -> &str {
    let part = version.split('.').nth(n).unwrap_or("0");
    let digits = part.trim_start_matches(|c: char| !c.is_ascii_digit());
    let end = digits.find(|c: char| !c.is_ascii_digit()).unwrap_or(digits.len());
    if end == 0 { "0" } else { &digits[..end] }
}

/// Who the compiler says it is.
fn identity(d: &mut Defs, target: &TargetInfo, opts: &Predef) {
    d.flag("__rucc__");
    d.set("__rucc_version__", &format!("\"{VERSION}\""));
    d.set("__rucc_major__", field(VERSION, 0));
    d.set("__rucc_minor__", field(VERSION, 1));
    d.set("__rucc_patchlevel__", field(VERSION, 2));
    // The promise from section 4.5. Everything in the matrix hangs off this line, except on an
    // MSVC row, where the headers are Microsoft's and `__GNUC__` sends them down paths written for
    // mingw. clang leaves it out there too, and `-fgnuc-version=` puts it back in both.
    let gcc = opts.gnuc_given || target.tuple.env() != tuple::Env::Msvc;
    if gcc {
        d.set("__GNUC__", &opts.gnuc.major.to_string());
        d.set("__GNUC_MINOR__", &opts.gnuc.minor.to_string());
        d.set("__GNUC_PATCHLEVEL__", &opts.gnuc.patch.to_string());
    }
    d.set("__VERSION__", &format!("\"rucc {VERSION}\""));
    // Not `__clang__`, deliberately. Section 4.5 says so, and a header that takes the Clang
    // path expects Clang's extension surface rather than GCC's.
    //
    // Which of the two readings of `inline` is in force, which a header reads to decide how to
    // write its own inline definitions: glibc's `__extern_inline` is `extern __inline` under the
    // one and adds `__attribute__ ((__gnu_inline__))` under the other. C99 changed the meaning of
    // the keyword and gcc follows the dialect, so the C89 ones keep GNU's reading and every
    // dialect after them takes C's until `-fgnu89-inline` says otherwise.
    let gnu_inline = opts.gnu89_inline || opts.std == Std::C89;
    d.flag_if(gcc && gnu_inline, "__GNUC_GNU_INLINE__");
    d.flag_if(gcc && !gnu_inline, "__GNUC_STDC_INLINE__");
    // The charsets a literal is converted to. Both are fixed here rather than settable, since
    // there is no `-fexec-charset` to set them with, and both are what gcc answers with none.
    // The wide one follows `wchar_t`, which is sixteen bits on Windows and thirty two
    // everywhere else, so it is the one target fact in this function.
    // gcc 11 is the first release that names them, so a persona that claims gcc 10 does not.
    let named = opts.gnuc.major >= 11;
    d.set_if(named, "__GNUC_EXECUTION_CHARSET_NAME", "\"UTF-8\"");
    let wide = if target.wchar_width == 16 { "\"UTF-16LE\"" } else { "\"UTF-32LE\"" };
    d.set_if(named, "__GNUC_WIDE_EXECUTION_CHARSET_NAME", wide);
    // The C++ ABI this would be if it compiled C++, which gcc defines in C as well. It is not
    // a claim about this compiler so much as a number headers read: libstdc++ is not the only
    // thing that tests it, and a C header shared with a C++ one reaches it through `extern
    // "C"` guards. The value is the one the claimed gcc release gives.
    d.set_if(gcc, "__GXX_ABI_VERSION", &gxx_abi_version(opts.gnuc.major).to_string());
    // gcc says this when it writes `.cfi_*` directives into its assembly, and a program that
    // writes its own assembly reads it to know that it may write them too. glibc's `sysdep.h`
    // is one. Every table here is written with those directives, so the answer follows whether
    // there is a table. It is no on 32-bit Arm, which has its own unwind tables, and off ELF,
    // where gcc says nothing: mingw-w64 has SEH, and clang for Apple does not say it. FreeBSD's
    // gcc for riscv64 says no as well, and so does its amd64 gcc under `-m32`, which builds no
    // unwind tables for i386, and the answer here is the same. A no only means that a program
    // leaves its own directives out.
    let elf = target.tuple.object_format() == tuple::ObjectFormat::Elf;
    let freebsd_quiet = target.tuple.os() == tuple::Os::FreeBsd
        && matches!(target.tuple.arch(), tuple::Arch::Riscv64 | tuple::Arch::X86);
    let arm = target.tuple.arch() == tuple::Arch::Arm;
    let cfi = gcc && opts.unwinds && elf && !arm && !freebsd_quiet;
    d.flag_if(cfi, "__GCC_HAVE_DWARF2_CFI_ASM");
    // gcc says this on the machines it has a speculation barrier for, where
    // `__builtin_speculation_safe_value` is the value and then the barrier. rucc-sema's list of
    // those machines is the same. clang has no such builtin, so the Apple and MSVC rows say nothing.
    let apple = matches!(target.tuple.os(), tuple::Os::MacOs | tuple::Os::IOs);
    let clang = apple || target.tuple.env() == tuple::Env::Msvc;
    let barrier = matches!(
        target.tuple.arch(),
        tuple::Arch::X86_64
            | tuple::Arch::X86
            | tuple::Arch::Aarch64
            | tuple::Arch::Arm
            | tuple::Arch::PowerPc64
    );
    d.flag_if(gcc && !clang && barrier, "__HAVE_SPECULATION_SAFE_VALUE");
}

/// `__GXX_ABI_VERSION` for a gcc release, which is a thousand and the `-fabi-version=` that
/// release uses when it is not given one.
///
/// The last seven are what gcc 10 to 16 give on NetBSD, FreeBSD and Ubuntu. The older ones
/// are the defaults the gcc manual lists for its releases.
const fn gxx_abi_version(major: u32) -> u32 {
    match major {
        0..=4 => 1002,
        5 => 1009,
        6 => 1010,
        7 => 1011,
        8 | 9 => 1013,
        10 => 1014,
        _ => 1005 + major,
    }
}

/// What the dialect flags say.
fn dialect(d: &mut Defs, target: &TargetInfo, opts: &Predef) {
    d.flag("__STDC__");
    d.set_if(opts.hosted, "__STDC_HOSTED__", "1");
    d.set_if(!opts.hosted, "__STDC_HOSTED__", "0");
    // A file of assembly is told so, and is not told which C it is or what its literals hold.
    // Those three are the whole of the difference gcc 16 makes between `-dM` for C and for
    // `assembler-with-cpp`, and every other name here stays, since kernel headers read the
    // machine's macros from `.S` files as much as from C.
    d.flag_if(opts.assembler, "__ASSEMBLER__");
    if let Some(version) = opts.std.stdc_version().filter(|_| !opts.assembler) {
        d.set("__STDC_VERSION__", version);
    }
    // Defined exactly when the extensions are off, which is the whole difference between
    // `-std=c23` and `-std=gnu23` as far as the preprocessor is concerned.
    d.flag_if(!opts.gnu_extensions, "__STRICT_ANSI__");
    // gcc says these where `u"..."` and `U"..."` are literals, which is gnu99 and every dialect
    // from C11 on. C89, gnu89 and strict C99 do not have them.
    let unicode = opts.std >= Std::C11 || (opts.std == Std::C99 && opts.gnu_extensions);
    d.flag_if(!opts.assembler && unicode, "__STDC_UTF_16__");
    d.flag_if(!opts.assembler && unicode, "__STDC_UTF_32__");
    // What glibc's `<pthread.h>` reads to spell `pthread_cleanup_push` with a `cleanup` attribute
    // rather than with `setjmp`, and gcc defines it in C for `-fexceptions` and for
    // `-fnon-call-exceptions` alone.
    d.flag_if(opts.exceptions, "__EXCEPTIONS");
    // Only while the arithmetic is IEC 60559's, which a fast math licence ends. glibc's
    // `<stdc-predef.h>` writes the same four from `__GCC_IEC_559` and writes none of them when that
    // is zero, so saying them here under `-ffast-math` would be the redefinition described below
    // with the opposite sign.
    // wasm32 says none of them, as clang does there. wasi-libc has no `<stdc-predef.h>` to say them
    // for it, and its maths library does not give the whole of Annex F.
    // The BSDs say none of these five either. gcc defines none of them itself, and on GNU/Linux
    // they come from the C library's `<stdc-predef.h>`, which gcc includes there and nowhere else.
    let bsd = target.tuple.os().is_bsd();
    let iec =
        opts.math.iec_559(opts.trapping_math) && target.tuple.arch() != tuple::Arch::Wasm32 && !bsd;
    d.flag_if(iec, "__STDC_IEC_559__");
    d.flag_if(iec, "__STDC_IEC_559_COMPLEX__");
    // TS 18661-1's date, in every dialect, which is gcc 16's answer rather than the standard's.
    // C23 folded that document into Annex F and gave the macro a date of its own, so 202311L is
    // the value C23 asks for, and writing it is what a reading of the standard alone produces.
    // It also breaks every translation unit that reaches glibc. `<stdc-predef.h>` is included
    // ahead of the first line of the file and defines this name as 201404L whenever
    // `__GCC_IEC_559` is positive, which it is here, so a different value is a redefinition with
    // a different body and that is a diagnostic on a line the program never wrote. The cost is
    // not only noise: sqlite's configure runs its feature tests through autosetup's `cctest
    // -nooutput 1`, which reads any output at all as a failed test, and the readline completion
    // test failed for no other reason than this warning.
    d.set_if(iec, "__STDC_IEC_60559_BFP__", "201404L");
    // The same date for the complex half, which is the other name `<stdc-predef.h>` writes and
    // which was missing here. The library makes the claim anyway: the `#else` in
    // `<stdc-predef.h>` is reached by a compiler that says nothing about its intent, and it
    // presumes an older compiler that meant yes, so saying nothing would not withhold anything,
    // it would only make the value arrive from somewhere else. And the claim is true, since the
    // complex arithmetic is lowered.
    d.set_if(iec, "__STDC_IEC_60559_COMPLEX__", "201404L");
    // A promise that every `wchar_t` holds a UCS code point in every locale, which glibc's
    // `<stdc-predef.h>` makes and Apple's library does not: a Mac `wchar_t` in a legacy locale
    // is not Unicode, and clang for Apple leaves the macro out for that reason. A program that
    // reads it to skip `mbstowcs` is the one that would be wrong.
    let apple = matches!(target.tuple.os(), tuple::Os::MacOs | tuple::Os::IOs);
    d.set_if(!apple && !bsd, "__STDC_ISO_10646__", "201706L");
    // The type behind `char8_t`, which C23 added and no dialect before it has. It sits here
    // rather than next to `__CHAR16_TYPE__` and `__CHAR32_TYPE__` because those two are the
    // same in every dialect and this one is not, which is the whole reason a header can test
    // for it: gcc's own `stdatomic.h` writes `atomic_char8_t` under `#ifdef __CHAR8_TYPE__`
    // and gets it in C23 and not in C17.
    d.set_if(opts.std >= Std::C23, "__CHAR8_TYPE__", "unsigned char");
    // C11 made atomics, threads, complex arithmetic and variable length arrays conditional, and
    // each has a `__STDC_NO_*` macro a compiler defines to say it has not got one. gcc 16 defines
    // none of them in any mode, and neither does this compiler, because all four are here: the
    // `_Atomic` type and `stdatomic.h`, the C library's `threads.h` with `_Thread_local` under
    // it, and the complex arithmetic are all lowered, and a claim that is not true changes what
    // a header declares rather than turning anything off. glibc's `regex.h` is the example of
    // that: it writes the bound of `regexec`'s match array as `_REGEX_NELTS (__nmatch)`, which
    // is the parameter when the dialect has variable length arrays and nothing at all when a
    // compiler says it does not.
    // What `__has_embed` answers with. They are defined in every dialect and not only in C23,
    // because the operator is answerable in every dialect and a header that writes
    // `#if __has_embed(...) == __STDC_EMBED_FOUND__` under `-std=gnu17` would otherwise be
    // comparing against zero and taking the not found branch on a resource that is there.
    // gcc 15 is the release that has `#embed`, and under an older persona `__has_embed` is not
    // an operator either, so the two go together.
    let embed = opts.gnuc.major >= 15;
    d.set_if(embed, "__STDC_EMBED_NOT_FOUND__", "0");
    d.set_if(embed, "__STDC_EMBED_FOUND__", "1");
    d.set_if(embed, "__STDC_EMBED_EMPTY__", "2");
}

/// The memory orders and the lock free answers.
///
/// These would be here whether or not `_Atomic` was, because they are the numbering the `__atomic` builtins take rather than a promise
/// about the language. musl's `stdatomic.h` writes `memory_order_relaxed = __ATOMIC_RELAXED`
/// with no test around it at all, so a compiler without them prints an enumerator whose value
/// is an identifier.
///
/// Two means always lock free, and every integer type gets a two on all three targets, which
/// are all sixty four bit machines. `long long` is the one that would change on a thirty two
/// bit target, where a double word load is an instruction the machine may or may not have.
fn atomics(d: &mut Defs, target: &TargetInfo, opts: &Predef) {
    d.set("__ATOMIC_RELAXED", "0");
    d.set("__ATOMIC_CONSUME", "1");
    d.set("__ATOMIC_ACQUIRE", "2");
    d.set("__ATOMIC_RELEASE", "3");
    d.set("__ATOMIC_ACQ_REL", "4");
    d.set("__ATOMIC_SEQ_CST", "5");
    // The gate is the machine word rather than `long`, because Windows has a thirty two bit
    // `long` on a sixty four bit machine and its `long long` is still one instruction. i386 is
    // the thirty two bit machine that says two anyway, because every processor it builds for
    // has `cmpxchg8b`, and gcc and clang both say two there.
    // wasm32 says two as well, because its values are sixty four bits wide and an `i64` atomic
    // is one instruction.
    let x86 = matches!(target.tuple.arch(), tuple::Arch::X86_64 | tuple::Arch::X86);
    let wasm = target.tuple.arch() == tuple::Arch::Wasm32;
    let llong = if target.pointer_width == 64 || x86 || wasm { "2" } else { "1" };
    // RISC-V has no atomic instruction narrower than a word, and gcc 13 is the first release that
    // writes the loop over a word for one rather than calling libatomic. Before it, the byte and
    // the half word are only sometimes lock free and have no `__sync` compare and swap.
    let riscv = matches!(target.tuple.arch(), tuple::Arch::Riscv64 | tuple::Arch::Riscv32);
    let narrow = if riscv && opts.gnuc.major < 13 { "1" } else { "2" };
    // A 32 bit x86 processor from before the Pentium has a narrower `cmpxchg` or none. The i386
    // has none, so nothing is always lock free and there is no `__sync` compare and swap, and
    // the i486 has it up to the word. That is what `-march=i386` says to the boot code.
    let swap = match opts.processor {
        Some(processor) if target.tuple.arch() == tuple::Arch::X86 => processor.compare_and_swap,
        _ => 8,
    };
    let (narrow, word) = if swap == 0 { ("1", "1") } else { (narrow, "2") };
    let llong = if swap < 8 { "1" } else { llong };
    for name in ["BOOL", "CHAR", "CHAR16_T", "SHORT"] {
        d.set(&format!("__GCC_ATOMIC_{name}_LOCK_FREE"), narrow);
    }
    for name in ["CHAR32_T", "WCHAR_T", "INT", "LONG", "POINTER"] {
        d.set(&format!("__GCC_ATOMIC_{name}_LOCK_FREE"), word);
    }
    // `char8_t` is a type of C23 alone, as `__CHAR8_TYPE__` is, and gcc 15 is the first release
    // that answers for it in C.
    let char8 = opts.std >= Std::C23 && opts.gnuc.major >= 15;
    d.set_if(char8, "__GCC_ATOMIC_CHAR8_T_LOCK_FREE", narrow);
    // The one that is not always two: a target whose word is thirty two bits wide can only
    // promise `long long` is lock free if it has a double word instruction, and the honest
    // answer there is sometimes rather than always.
    d.set("__GCC_ATOMIC_LLONG_LOCK_FREE", llong);
    d.set("__GCC_ATOMIC_TEST_AND_SET_TRUEVAL", "1");
    // What `__sync_bool_compare_and_swap` works on, one macro per width in bytes. Every target
    // here has it at all four, apart from the narrow widths that RISC-V gcc 12 leaves to
    // libatomic and the widths an old x86 processor has no `cmpxchg` for. glibc reads these
    // rather than the `__atomic_*` set because they are the older question and the answer is
    // the same one.
    let widths: &[u32] = if narrow == "1" { &[4, 8] } else { &[1, 2, 4, 8] };
    for width in widths.iter().filter(|&&width| width <= swap) {
        d.flag(&format!("__GCC_HAVE_SYNC_COMPARE_AND_SWAP_{width}"));
    }
    // AArch64 has it at sixteen bytes as well, as the exclusive pair loop or `caspal`, and gcc
    // and clang both say so on every AArch64 target. x86-64 says so only with `cx16`, which is
    // the extension's own macro.
    let aarch64 = target.tuple.arch() == tuple::Arch::Aarch64;
    d.flag_if(aarch64, "__GCC_HAVE_SYNC_COMPARE_AND_SWAP_16");
    // The two flag bits an x86 memory order can carry, for the hardware lock elision prefixes.
    // They are numbers a program passes back to a builtin rather than a claim that the prefix
    // is emitted, and a program that computes one on a machine where the macro is missing gets
    // a preprocessor error rather than a slower atomic.
    if x86 {
        d.set("__ATOMIC_HLE_ACQUIRE", "65536");
        d.set("__ATOMIC_HLE_RELEASE", "131072");
    }
}

/// What the optimizer level says.
fn optimization(d: &mut Defs, opts: &Predef) {
    d.flag_if(opts.opt_level.runs_optimizer(), "__OPTIMIZE__");
    d.flag_if(opts.opt_level.is_size(), "__OPTIMIZE_SIZE__");
    // glibc's headers test this before deciding whether to define a function as an inline
    // wrapper, so getting it wrong changes what a program links against.
    d.flag_if(!opts.opt_level.runs_optimizer(), "__NO_INLINE__");
    // Zero, and one under `-ffinite-math-only`, which `-ffast-math` implies. glibc's `math.h`
    // reads it to decide whether to declare the `__*_finite` aliases, so it has to be defined
    // rather than merely not claimed: a header testing `#if __FINITE_MATH_ONLY__ > 0` on a
    // compiler that leaves it undefined takes the same branch, but one writing `#if
    // !__FINITE_MATH_ONLY__` is a different question and gcc gives it an answer.
    let math = &opts.math;
    let trapping = opts.trapping_math;
    d.set("__FINITE_MATH_ONLY__", if math.finite_only { "1" } else { "0" });
    // One macro per licence, each defined only when it was given, which is how gcc 16 spells
    // them. `__FAST_MATH__` is all of them at once and is worked out rather than remembered from
    // the flag, so `-ffast-math -ftrapping-math` does not claim it, and regrouping is the one gcc
    // drops unless nothing could tell it happened.
    d.flag_if(math.fast(trapping), "__FAST_MATH__");
    d.flag_if(!math.errno, "__NO_MATH_ERRNO__");
    d.flag_if(!trapping, "__NO_TRAPPING_MATH__");
    d.flag_if(!math.signed_zeros, "__NO_SIGNED_ZEROS__");
    d.flag_if(math.reciprocal, "__RECIPROCAL_MATH__");
    d.flag_if(math.associative(trapping), "__ASSOCIATIVE_MATH__");
}

/// What gcc says on x86 in both widths, beside the processor and its extensions.
///
/// The two named address spaces and `__float80` are there in every dialect, although `__seg_fs`
/// and `__seg_gs` are keywords in the GNU ones only. That is gcc 13 on Linux, and the BSD rows of
/// the reference say the same. clang has its own list, so the Apple and MSVC rows keep theirs.
///
/// NetBSD's gcc is configured to tune for `nocona`, and says so when the command line names no
/// processor. A `-march=` or a `-mtune=` replaces the default, as the driver specs of that gcc have
/// it.
fn x86(d: &mut Defs, triple: &Triple, opts: &Predef) {
    let clang = triple.os == Os::Darwin || triple.env == Env::Msvc;
    if !clang {
        d.flag("__SEG_FS");
        d.flag("__SEG_GS");
        let float80 = if triple.arch == Arch::X86_64 { "16" } else { "12" };
        d.set("__SIZEOF_FLOAT80__", float80);
    }
    d.flag_if(triple.os == Os::NetBsd && !opts.tuned, "__tune_nocona__");
    d.flag_if(!clang && !opts.x87, "_SOFT_FLOAT");
}

/// The architecture, the operating system and the object format.
fn platform(d: &mut Defs, target: &TargetInfo, opts: &Predef) {
    // The macros a target with no backend predefines are not written down here. They are a
    // header's whole view of the machine, a wrong one is a header taking a branch written for
    // another processor, and there is nothing cheap that would catch it. The driver takes a three
    // field triple, so a target this cannot spell is one nobody can ask for yet rather than a
    // hole in what it answers.
    // The riscv32 row, which a riscv64 target builds the compat vDSO's assembly for.
    if target.tuple.arch() == rucc_tuple::Arch::Riscv32 {
        riscv(d, opts, 32);
        return;
    }
    let Some(triple) = Triple::from_tuple(target.tuple) else {
        return;
    };
    match triple.arch {
        Arch::X86_64 => {
            d.flag("__x86_64__");
            d.flag("__x86_64");
            d.flag("__amd64__");
            d.flag("__amd64");
            // Flag outputs, `"=@ccz"`, which the kernel's `CC_SET` uses when this is defined and
            // replaces with a `set<cond>` of its own when it is not.
            d.flag("__GCC_ASM_FLAG_OUTPUTS__");
            // One macro per extension the unit is built for, which is `__SSE__`, `__SSE2__`,
            // `__MMX__` and `__FXSR__` for the baseline every x86-64 has, and `__SSE4_2__` and
            // its relatives for what `-march=` and `-msse4.2` add. Only the extensions this
            // compiler has the intrinsics for get one, see `rucc_target::isa`.
            for name in opts.isa.macros() {
                d.flag(&name);
            }
            // Floating point is done in the vector registers for as long as there are some. gcc
            // takes both macros away with `-mno-sse`, which is how the kernel's headers find out.
            let has = |name| Feature::named(name).is_some_and(|it| opts.isa.has(it));
            if has("sse") {
                d.flag("__SSE_MATH__");
            }
            if has("sse2") {
                d.flag("__SSE2_MATH__");
            }
            d.flag("__k8");
            d.flag("__k8__");
            x86(d, &triple, opts);
            // Which code model, which is the small one unless `-mcmodel=kernel` said otherwise.
            // A kernel header reads the second to know its addresses are in the top 2 GiB.
            d.flag(match opts.code_model {
                CodeModel::Small | CodeModel::Tiny | CodeModel::Medany => "__code_model_small__",
                CodeModel::Kernel => "__code_model_kernel__",
            });
            // The MMX registers are not used on x86-64: the sixty four bit operations go
            // through SSE instead. gcc's own `xmmintrin.h` reads this to decide how to write
            // `_mm_maskmove_si64`, so a compiler that leaves it undefined is handed a
            // different function body than gcc is, which is what the header sweep found.
            if has("sse2") {
                d.flag("__MMX_WITH_SSE__");
            }
            // A fused multiply add is one instruction in a unit built for `fma`, so the same
            // macros as on AArch64. gcc defines them here and clang does not, so an Apple or an
            // MSVC row leaves them out.
            let clang = triple.os == Os::Darwin || triple.env == Env::Msvc;
            if has("fma") && opts.vector && !clang {
                for name in ["", "F", "F32", "F64", "F32x"] {
                    d.set(&format!("__FP_FAST_FMA{name}"), "1");
                }
            }
        }
        Arch::Aarch64 => {
            d.flag("__aarch64__");
            d.flag("__AARCH64EL__");
            d.flag("__GCC_ASM_FLAG_OUTPUTS__");
            d.set("__ARM_ARCH", "8");
            // gcc writes the profile and the float formats as numbers, and clang as a character
            // and in hex. Both are the same value to an `#if`.
            let clang = triple.os == Os::Darwin || triple.env == Env::Msvc;
            d.set("__ARM_ARCH_PROFILE", if clang { "'A'" } else { "65" });
            d.set("__ARM_64BIT_STATE", "1");
            d.set("__ARM_ALIGN_MAX_PWR", "28");
            if opts.vector {
                d.set("__ARM_FP", if clang { "0xe" } else { "14" });
                d.set("__ARM_NEON", "1");
            }
            // `__fp16` is the IEEE half, and a function may take one and return one. gcc says so
            // only where there are floating registers to pass one in, and clang says so always.
            if opts.vector || clang {
                d.set("__ARM_FP16_FORMAT_IEEE", "1");
                d.set("__ARM_FP16_ARGS", "1");
            }
            if !opts.strict_align {
                d.set("__ARM_FEATURE_UNALIGNED", "1");
            }
            d.set("__ARM_PCS_AAPCS64", "1");
            // The rest of what gcc says for a plain Armv8-A. Every one is an instruction the
            // base architecture has, so none of them depends on a `-march` this compiler does
            // not take yet, and a program that tests one to choose `__builtin_clz` or a
            // hardware divide over a portable loop takes the same branch it takes under gcc.
            d.set("__ARM_ARCH_8A", "1");
            d.set("__ARM_ARCH_ISA_A64", "1");
            d.set("__ARM_FEATURE_CLZ", "1");
            if opts.vector {
                d.set("__ARM_FEATURE_FMA", "1");
                d.set("__ARM_FEATURE_NUMERIC_MAXMIN", "1");
            }
            d.set("__ARM_FEATURE_IDIV", "1");
            d.set("__ARM_ALIGN_MAX_STACK_PWR", "16");
            d.set("__ARM_SIZEOF_MINIMAL_ENUM", "4");
            // The target's own `wchar_t`, which is two bytes on Windows and four everywhere else.
            // clang for aarch64-w64-mingw32 says 2 there, and a header that sizes a buffer by it
            // rather than by `sizeof` gets half the room it asked for when this says 4.
            d.set("__ARM_SIZEOF_WCHAR_T", &wchar(target).size.to_string());
            match opts.code_model {
                CodeModel::Tiny => d.set("__AARCH64_CMODEL_TINY__", "1"),
                _ => d.set("__AARCH64_CMODEL_SMALL__", "1"),
            }
            // A fused multiply add is one instruction here, and glibc's `math.h` turns these
            // into `FP_FAST_FMA` and `FP_FAST_FMAF`, which a program reads to decide whether
            // calling `fma` is cheaper than writing the product and the sum apart.
            for name in ["", "F", "F32", "F64", "F32x"].into_iter().filter(|_| opts.vector) {
                d.set(&format!("__FP_FAST_FMA{name}"), "1");
            }
            // The CRC32 instructions, which are optional in Armv8.0 and part of every architecture
            // after it. gcc says so for `-march=armv8-a+crc` and for `armv8.1-a` and later, and
            // not for plain `armv8-a`, and PostgreSQL's checksum is one of the programs that looks
            // before it includes `<arm_acle.h>`.
            if Feature::aarch64("crc").is_some_and(|crc| opts.isa.has(crc)) {
                d.set("__ARM_FEATURE_CRC32", "1");
            }
            // The large system extensions, which come with Armv8.1-A, as gcc spells them.
            if Feature::aarch64("lse").is_some_and(|lse| opts.isa.has(lse)) {
                d.set("__ARM_FEATURE_ATOMICS", "1");
            }
            // What `-mbranch-protection=` asked for, as the ACLE spells it. The signing one is a
            // set of bits: the A key is bit 0 and signing in leaf functions too is bit 2.
            if opts.branch_protection.bti {
                d.set("__ARM_FEATURE_BTI_DEFAULT", "1");
            }
            match opts.branch_protection.sign {
                SignReturn::None => {}
                SignReturn::NonLeaf => d.set("__ARM_FEATURE_PAC_DEFAULT", "1"),
                SignReturn::All => d.set("__ARM_FEATURE_PAC_DEFAULT", "5"),
            }
            if opts.branch_protection.gcs {
                d.set("__ARM_FEATURE_GCS_DEFAULT", "1");
            }
            if !clang && opts.gnuc.major >= 14 {
                sme(d);
            }
        }
        Arch::Riscv64 => riscv(d, opts, 64),
        Arch::X86 => {
            d.flag("__i386__");
            d.flag("__i386");
            match opts.processor {
                // The processor the target's gcc builds for, or the one `-march=` named, and the
                // extensions it has. The amd64 gcc of a BSD builds `-m32` code for the K8, with
                // its SSE2, and `-march=i386` takes all of them away, which is how the boot code
                // and the lib32 libraries find out what they may use.
                Some(processor) => {
                    for name in processor.arch.iter().chain(opts.tune) {
                        d.flag(name);
                    }
                    for name in opts.isa.macros() {
                        d.flag(&name);
                    }
                }
                // The processor the unit is built for, which gcc spells both as the family and as
                // the product. The backend writes SSE2 for floating point, so the product is the
                // Pentium 4 rather than the i686 that a plain `i686-linux-gnu-gcc` says, and a
                // header choosing an intrinsic by processor takes the branch for what the code
                // runs on.
                None => {
                    d.flag("__i686__");
                    d.flag("__i686");
                    d.flag("__pentium4__");
                    d.flag("__pentium4");
                    // The same four extensions as the x86-64 baseline, because they are what the
                    // Pentium 4 has and what the backend emits. `__SSE2_MATH__` is the promise
                    // that `double` arithmetic happens in an SSE register, which is also why
                    // `__FLT_EVAL_METHOD__` is zero here unless `-mfpmath=387` makes it two.
                    for name in Isa::baseline().macros() {
                        d.flag(&name);
                    }
                    // What the unit has over the x86-64 baseline, which is `__LAHF_SAHF__` for
                    // the Pentium 4 and `__SSE4_2__` and its relatives for the flags, as on
                    // x86-64. The four above stay defined when the kernel turns them off, which
                    // is what they did before any flag was honoured here.
                    for name in opts.isa.minus(Isa::baseline()).macros() {
                        d.flag(&name);
                    }
                }
            }
            // The promise that `float` and `double` arithmetic is done in an SSE register, which
            // gcc takes away with `-mfpmath=387`.
            if !opts.x87_floats {
                d.flag("__SSE_MATH__");
                d.flag("__SSE2_MATH__");
            }
            d.flag("__code_model_32__");
            // gcc defines the fused multiply add macros under `-m32 -mfma` only with
            // `-mfpmath=sse`, because its default does `double` arithmetic on the x87 stack. The
            // backend here does it in SSE unless `-mfpmath=387` says otherwise, and writes
            // `vfmadd` in a unit built for `fma`, so the macros follow `fma` and the two.
            let has = |name| Feature::named(name).is_some_and(|it| opts.isa.has(it));
            if has("fma") && opts.vector && !opts.x87_floats && triple.env != Env::Msvc {
                for name in ["", "F", "F32", "F64", "F32x"] {
                    d.set(&format!("__FP_FAST_FMA{name}"), "1");
                }
            }
            if opts.gnu_extensions {
                d.flag("i386");
            }
            d.flag("__GCC_ASM_FLAG_OUTPUTS__");
            x86(d, &triple, opts);
        }
        // clang defines all four spellings, and wasi-libc's `wasi/api.h` stops with `#error`
        // without `__wasm32__`.
        Arch::Wasm32 => {
            d.flag("__wasm");
            d.flag("__wasm__");
            d.flag("__wasm32");
            d.flag("__wasm32__");
            // One macro for each feature the unit is built for, and on wasip3 for the features
            // that the preview turns on whatever the set is.
            let mut features = opts.wasm;
            if let Os::Wasi(preview) = triple.os {
                features = features.union(wasm::required(preview));
            }
            for name in features.macros() {
                d.flag(&name);
            }
            // wasi-libc's `setjmp.h` stops with `#error` unless exception handling is on. rucc
            // lowers `setjmp` with `exnref` on every WASI row and marks the feature only on a
            // function that calls it, so the macro is defined there with no flag (decision D5).
            // clang defines it only for `-mexception-handling`. wasm32-none has no libsetjmp.
            if matches!(triple.os, Os::Wasi(_)) && !features.has(wasm::Feature::ExceptionHandling) {
                d.flag("__wasm_exception_handling__");
            }
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
            // No `__unix__`, `__unix` or `unix`. clang for Apple defines none of them, and code
            // that tests `__unix__` before `__APPLE__` takes the path written for Linux, where
            // it reaches for `<linux/...>` headers or `/proc` and fails at build time or at
            // run time. What a Mac is identified by is `__APPLE__` and `__MACH__`.
            d.flag("__APPLE__");
            d.flag("__MACH__");
            d.set("__APPLE_CC__", "6000");
            d.set("__DYNAMIC__", "1");
            if triple.arch == Arch::Aarch64 {
                // Apple's own spelling of the architecture, which its headers use rather than
                // __aarch64__. sys/cdefs.h tests for it by name and reaches an #error called
                // "Unsupported architecture" without it, so every system header on this
                // platform fails on the first include until these two are here.
                d.flag("__arm64__");
                d.flag("__arm64");
                // Older Apple spellings that the SDK still reads. `arm/arch.h` sets its own
                // `_ARM_ARCH_*` family from `__ARM64_ARCH_8__`, and a header that tests
                // `__ARM_NEON__` rather than `__ARM_NEON` is taking the portable path without
                // the Advanced SIMD one. clang defines all three for every Apple arm64 target.
                d.flag("__ARM64_ARCH_8__");
                d.flag("__ARM_NEON__");
                d.flag("__AARCH64_SIMD__");
            }
            // clang says this on every little endian target and gcc says it on none, which is
            // why it is here rather than beside the byte order macros. Apple's headers were
            // written for clang alone, and `CFByteOrder.h` and `architecture/byte_order.h` both
            // choose their swaps from it.
            d.flag("__LITTLE_ENDIAN__");
            deployment_target(d, target);
        }
        Os::Windows if triple.env == Env::Msvc => msvc(d, target, triple.arch, opts),
        Os::Windows => {
            // Every spelling gcc has for this platform, because the mingw-w64 tree reads more
            // than one of them and a missing one is a declaration that quietly is not there.
            // `winuser.h` guards `EndTask` with `#ifdef WINNT` and `rpcdcep.h` guards six
            // declarations with `#ifndef WINNT`, so a compiler that leaves it undefined
            // preprocesses `windows.h` to a different set of functions than gcc does, which is
            // what the token comparison against a real mingw install found.
            d.flag("_WIN32");
            d.flag("__WIN32");
            d.flag("__WIN32__");
            d.flag("__WINNT");
            d.flag("__WINNT__");
            d.flag("__MINGW32__");
            // Not the machine word: `_WIN64` says the pointer is sixty four bits wide, and
            // i686-w64-mingw32-gcc defines neither it nor `__MINGW64__`.
            if target.pointer_width == 64 {
                d.flag("_WIN64");
                d.flag("__WIN64");
                d.flag("__WIN64__");
                d.flag("__MINGW64__");
            }
            // Which kind of exception machinery the platform has, and on this one there is only
            // the one: a table the operating system reads rather than anything the prologue
            // registers. mingw's `setjmp.h` reads this to choose the two argument `_setjmp`,
            // whose second argument is `__builtin_frame_address(0)` and becomes the frame
            // `longjmp` asks `RtlUnwindEx` to unwind to. That is the same address the function's
            // own unwind record reports, because the record names the frame pointer with an
            // offset of zero and the prologue leaves the pointer holding the body's stack
            // pointer, so the number the builtin answers with and the number the walk arrives at
            // are the same number by construction. Only for the architecture whose records this
            // compiler writes: x86_64-w64-mingw32-gcc defines it and i686-w64-mingw32-gcc does
            // not, because a thirty two bit Windows unwinds some other way.
            if triple.arch == Arch::X86_64 && target.pointer_width == 64 {
                d.flag("__SEH__");
            }
            // The processor as the Windows headers ask about it. `malloc.h`, `winnt.h` and the
            // interlocked functions branch on `_X86_` rather than on `__i386__`, and
            // i686-w64-mingw32-gcc defines it.
            if triple.arch == Arch::X86 {
                d.flag("_X86_");
            }
            // The family of C runtimes mingw-w64 targets, which is Microsoft's whichever DLL it
            // is, so gcc defines this for a UCRT toolchain as well as a msvcrt one. Which of the
            // two the headers are for is not a predefine: the sysroot's `_mingw.h` sets
            // `__MSVCRT_VERSION__` to what it was configured with, 0xE00 in the UCRT sysroot this
            // compiler fetches and 0x600 in the `-msvcrt` one, and defines `_UCRT` from that, the
            // same as it does under gcc.
            d.flag("__MSVCRT__");
            // The widest integer the compiler has, which is what Microsoft's headers ask
            // instead of asking about `long long`.
            d.set("_INTEGRAL_MAX_BITS", "64");
            // The unarmoured three, which are not reserved identifiers, so a strict mode may
            // not define them and gcc does not. Windows code tests all three anyway, the same
            // way portable Unix code still tests `linux`.
            if opts.gnu_extensions {
                d.flag("WIN32");
                d.flag("WINNT");
                if target.pointer_width == 64 {
                    d.flag("WIN64");
                }
            }
            windows_spellings(d, opts);
        }
        // The BSDs say what gcc's `TARGET_OS_CPP_BUILTINS` says for each, and `__ELF__` from
        // `elfos.h`. FreeBSD and DragonFly name `unix` with `builtin_define_std`, which gives
        // `__unix` and `__unix__` and gives `unix` outside a strict mode. NetBSD and OpenBSD
        // define `__unix__` alone. `__FreeBSD__` is the major release, which gcc takes from the
        // triple it was configured for.
        Os::FreeBsd(release) => {
            d.set("__FreeBSD__", &release.to_string());
            unix(d, opts);
            d.flag("__KPRINTF_ATTRIBUTE__");
            d.flag("__ELF__");
        }
        Os::DragonFly => {
            d.flag("__DragonFly__");
            unix(d, opts);
            d.flag("__ELF__");
        }
        // NetBSD's own gcc, which is the compiler its build uses and the only one with a RISC-V
        // port. It adds `__syslog_attribute__` to upstream's two names, and `<sys/cdefs.h>`
        // reads it to write `__sysloglike` with the `syslog` format. On RISC-V it adds
        // `__riscv__`, and `_SOFT_FLOAT` on a machine with no float registers.
        Os::NetBsd => {
            d.flag("__NetBSD__");
            d.flag("__unix__");
            d.flag("__syslog_attribute__");
            if triple.arch == Arch::Riscv64 {
                d.flag("__riscv__");
                let isa = RiscvIsa::new(opts.riscv_arch.as_deref(), opts.gnuc.major);
                d.flag_if(!isa.has("f"), "_SOFT_FLOAT");
            }
            d.flag("__ELF__");
        }
        Os::OpenBsd => {
            d.flag("__OpenBSD__");
            d.flag("__unix__");
            d.flag("__ANSI_COMPAT");
            d.flag("__ELF__");
        }
        // kencc defines no macro that names the system, and the Plan 9 headers test none. The
        // objects are not ELF, so `__ELF__` is not here either.
        Os::Plan9 => {}
        Os::None => {
            // Freestanding. `__ELF__` still holds, because the object format is a property of
            // the target rather than of having an operating system under it, and a wasm module
            // is not ELF.
            d.flag_if(triple.object_format() == ObjectFormat::Elf, "__ELF__");
        }
        // WASI is not Unix, so there is no `__unix__`. The wasi-libc headers select their
        // declarations by the preview macro, and `errno.h` reads it before it includes anything.
        Os::Wasi(preview) => {
            d.flag("__wasi__");
            d.flag(&format!("__wasip{}__", preview.number()));
            // wasip3 reaches the stack pointer and the TLS base through library calls, so that
            // each cooperative thread has its own, and clang 23 says so with this macro.
            d.flag_if(preview == Preview::P3, "__wasm_libcall_thread_context__");
        }
    }
    match triple.env {
        Env::Musl => d.flag("__musl__"),
        Env::Gnu | Env::None | Env::Msvc => {}
    }
    // The version of the libc's headers, and only when they are the tree we bundle. One tree serves
    // every glibc release with the differences written as `#if __GLIBC_MINOR__ >= n` inside the
    // files, so the release is the part of it the target supplies and the compiler is what supplies
    // it. Zig patches the same macro into the same tree the same way, which is where the spelling
    // comes from rather than from a scheme of ours: `__GLIBC_PREREQ` reads it and so does every
    // autoconf probe ever written.
    //
    // The condition is the whole of it and it is not here. A host glibc defines this macro in its
    // own `features.h` and so does a tree the user named, and a second definition with a different
    // value is a warning on every compilation, so the driver decides and this writes down what it
    // decided.
    if let Some(minor) = opts.glibc_minor {
        d.set("__GLIBC_MINOR__", &minor.to_string());
    }
    // LP64 is the model everywhere except Windows, and a great deal of code tests for it
    // rather than testing pointer and long widths separately.
    if target.long_width == 64 && target.pointer_width == 64 {
        d.flag("__LP64__");
        d.flag("_LP64");
    }
    // Its thirty two bit counterpart, which gcc and clang say for i686 on Linux and on Windows
    // and for x32. A program that tests `__ILP32__` to pick a 32-bit syscall layout gets the one
    // it wrote for this machine.
    if target.long_width == 32 && target.pointer_width == 32 {
        d.flag("__ILP32__");
        d.flag("_ILP32");
    }
    // What the assembler prepends to a C name to get the symbol. Mach-O keeps the leading
    // underscore that every a.out toolchain had and ELF dropped it. It has to be defined even
    // where it is empty, because of how it is used: glibc writes `__asm__ (__ASMNAME (name))`
    // and that stringifies `__USER_LABEL_PREFIX__`, so a compiler that leaves it undefined
    // does not get an error, it gets the name of the macro as the string and renames the
    // function.
    // COFF on i386 is the other one that kept it, as part of what cdecl is there.
    let underscored =
        triple.os == Os::Darwin || (triple.os == Os::Windows && triple.arch == Arch::X86);
    d.set("__USER_LABEL_PREFIX__", if underscored { "_" } else { "" });
    // Its counterpart, what the assembler puts in front of a register name. Empty on every
    // target here, since all three assemble in a syntax that does not mark registers, and
    // defined anyway for the same reason as the line above: it is used inside a stringize.
    d.set("__REGISTER_PREFIX__", "");
    // That `#pragma redefine_extname` is read, which a header asks before it writes one rather
    // than an `__asm__` label on each declaration. gcc defines it on every target.
    d.set("__PRAGMA_REDEFINE_EXTNAME", "1");

    // Position independent code is the default on the ELF targets and on Apple's, which is
    // what a distribution build expects. The value 2 is GCC's for `-fPIC` rather than `-fpic`.
    //
    // Both are defined whichever link the output is for, because `__PIC__` says there are no
    // absolute addresses in the text and that is true either way. What says which link it is is
    // `__PIE__`, and a program reads it to find out whether a name it exports is one something
    // else may replace. gcc defines it under `-fPIE` and under the default on a distribution
    // where the default is an executable, which is the default here too.
    //
    // Neither pair under `-fno-pic` and `-fno-pie`, which is what gcc does and what the claim
    // above stops being true for.
    // rucc writes no position independent wasm, so neither pair is defined there.
    if !matches!(triple.os, Os::Windows) && !triple.arch.is_wasm() && opts.pic != Pic::Absolute {
        d.set("__PIC__", "2");
        d.set("__pic__", "2");
        if opts.pic == Pic::Executable {
            d.set("__PIE__", "2");
            d.set("__pie__", "2");
        }
    }
}

/// What gcc's `builtin_define_std ("unix")` defines: `__unix` and `__unix__`, and `unix` too
/// unless a strict mode keeps the name for the program.
/// The keyword attributes of the AArch64 Scalable Matrix Extension, which gcc 14 and later
/// define on every AArch64 target, with SME or without it.
///
/// Each one is a scoped attribute in the `arm` namespace, and `__extension__` keeps a C mode
/// without `[[...]]` from warning about it. The two `__ARM_STATE_` macros say that the compiler
/// knows the state, so that a header can write `__arm_inout("za")`. The checker reads the
/// attributes as gcc built without SME does, and refuses what needs SME to build. gcc also defines
/// `__ARM_NEON_SVE_BRIDGE` here, which is a promise of `<arm_neon_sve_bridge.h>` and the SVE
/// types, and this compiler has neither yet, so it is left out. tamnd/rucc#4225.
fn sme(d: &mut Defs) {
    d.set("__ARM_STATE_ZA", "1");
    d.set("__ARM_STATE_ZT0", "1");
    for name in ["streaming", "streaming_compatible", "locally_streaming"] {
        d.set(&format!("__arm_{name}"), &format!("[[__extension__ arm::{name}]]"));
    }
    for name in ["new", "in", "out", "inout", "preserves"] {
        d.set(
            &format!("__arm_{name}(...)"),
            &format!("[[__extension__ arm::{name}(__VA_ARGS__)]]"),
        );
    }
}

fn unix(d: &mut Defs, opts: &Predef) {
    d.flag("__unix");
    d.flag("__unix__");
    d.flag_if(opts.gnu_extensions, "unix");
}

/// The RISC-V macros, for a machine whose registers are `xlen` bits wide.
///
/// The extensions come from `-march=`, which gcc reads for the macros and the ABI does not change.
/// The kernel's `-mabi=lp64` with the default machine still has the D extension, so gcc gives
/// `__riscv_flen` and the fused multiply add macros there, and says the float ABI is soft.
fn riscv(d: &mut Defs, opts: &Predef, xlen: u32) {
    let isa = RiscvIsa::new(opts.riscv_arch.as_deref(), opts.gnuc.major);
    d.flag("__riscv");
    d.set("__riscv_xlen", &xlen.to_string());
    if isa.has("d") {
        d.set("__riscv_flen", "64");
    } else if isa.has("f") {
        d.set("__riscv_flen", "32");
    }
    d.flag(if opts.soft_float { "__riscv_float_abi_soft" } else { "__riscv_float_abi_double" });
    if isa.has("f") {
        d.flag("__riscv_fdiv");
        d.flag("__riscv_fsqrt");
    }
    // A fused multiply add is one instruction for each float width the machine has.
    let fma: &[&str] = match (isa.has("f"), isa.has("d")) {
        (_, true) => &["", "F", "F32", "F64", "F32x"],
        (true, false) => &["F", "F32"],
        _ => &[],
    };
    for name in fma {
        d.set(&format!("__FP_FAST_FMA{name}"), "1");
    }
    if isa.has("m") {
        d.flag("__riscv_mul");
        d.flag("__riscv_div");
        d.flag("__riscv_muldiv");
    }
    d.flag_if(isa.has("a"), "__riscv_atomic");
    d.flag_if(isa.has("c"), "__riscv_compressed");
    // gcc 12 is the first release with a macro for each extension, with the version of the
    // extension as its value, and gcc 14 the first with the tuning macro.
    if opts.gnuc.major >= 12 {
        d.flag("__riscv_arch_test");
        for (name, version) in &isa.extensions {
            d.set(&format!("__riscv_{name}"), &version.to_string());
        }
    }
    d.flag_if(opts.gnuc.major >= 14, "__riscv_misaligned_slow");
    // Position independent code is gcc's own code model. gcc 10 calls it `pic`, gcc 11 and 12 call
    // it `pic` and `medany`, and gcc 13 calls it `medany` alone. The kernel's `setup_vm` reads
    // `medany`, since it runs before the kernel is where it was linked.
    let pic = opts.pic != Pic::Absolute;
    d.flag_if(pic && opts.gnuc.major < 13, "__riscv_cmodel_pic");
    if pic && opts.gnuc.major < 11 {
        // `pic` alone.
    } else if pic || opts.code_model == CodeModel::Medany {
        d.set("__riscv_cmodel_medany", "1");
    } else {
        d.set("__riscv_cmodel_medlow", "1");
    }
}

/// The RISC-V extensions a unit is built for, with the version gcc gives each in its macro.
struct RiscvIsa {
    extensions: Vec<(&'static str, u32)>,
}

impl RiscvIsa {
    /// The extensions `-march=` names, as gcc reads them for its macros.
    ///
    /// The single letters come after `rv64` or `rv32` and before the first `_`, and the longer
    /// names come after each `_`. `g` is `imafd` with `zicsr` and `zifencei`. A version written
    /// after a name, as in `i2p1`, is skipped, and so is a name this compiler has no version for.
    /// F needs `zicsr` and D needs F, and gcc 15 also names the parts that `a`, `c` and `m` are
    /// made of. With no `-march=`, the machine is `rv64gc`.
    fn new(march: Option<&str>, gcc: u32) -> RiscvIsa {
        let march = march.unwrap_or("rv64gc");
        let rest = march.strip_prefix("rv64").or_else(|| march.strip_prefix("rv32"));
        let rv32 = march.starts_with("rv32");
        let mut parts = rest.unwrap_or("gc").split('_');
        let mut names: Vec<&str> = Vec::new();
        let letters = parts.next().unwrap_or_default();
        let mut chars = letters.char_indices().peekable();
        while let Some((at, c)) = chars.next() {
            if c.is_ascii_digit() {
                // A version, `2p1` or `2`, which belongs to the letter before it.
                while chars.next_if(|(_, c)| c.is_ascii_digit() || *c == 'p').is_some() {}
                continue;
            }
            if c == 'g' {
                names.extend(["i", "m", "a", "f", "d", "zicsr", "zifencei"]);
            } else {
                names.push(&letters[at..at + c.len_utf8()]);
            }
        }
        names.extend(parts.map(without_version));
        let has = |names: &[&str], name: &str| names.contains(&name);
        if has(&names, "d") {
            names.push("f");
        }
        if has(&names, "f") {
            names.push("zicsr");
        }
        if gcc >= 15 {
            if has(&names, "a") {
                names.extend(["zaamo", "zalrsc"]);
            }
            if has(&names, "m") {
                names.push("zmmul");
            }
            if has(&names, "c") {
                names.push("zca");
                if has(&names, "d") {
                    names.push("zcd");
                }
                if rv32 && has(&names, "f") {
                    names.push("zcf");
                }
            }
        }
        let mut extensions: Vec<(&'static str, u32)> = Vec::new();
        for name in names {
            if let Some(&known) = RISCV_VERSIONS.iter().find(|(known, _)| *known == name) {
                if !extensions.contains(&known) {
                    extensions.push(known);
                }
            }
        }
        RiscvIsa { extensions }
    }

    fn has(&self, name: &str) -> bool {
        self.extensions.iter().any(|(known, _)| *known == name)
    }
}

/// A longer extension name without the version written after it, which is `2` or `2p0`.
fn without_version(part: &str) -> &str {
    let digits =
        |text: &str| text.len() - text.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    let minor = digits(part);
    let rest = &part[..part.len() - minor];
    match rest.strip_suffix('p') {
        Some(major) if minor > 0 && digits(major) > 0 => &major[..major.len() - digits(major)],
        _ => rest,
    }
}

/// The version of each extension that gcc gives as the value of its macro, major times a million
/// plus minor times a thousand. They are those of the 20191213 specification, which is gcc's
/// default from gcc 12 on.
const RISCV_VERSIONS: &[(&str, u32)] = &[
    ("i", 2001000),
    ("e", 2000000),
    ("m", 2000000),
    ("a", 2001000),
    ("f", 2002000),
    ("d", 2002000),
    ("c", 2000000),
    ("h", 1000000),
    ("zicsr", 2000000),
    ("zifencei", 2000000),
    ("zihintpause", 2000000),
    ("zaamo", 1000000),
    ("zalrsc", 1000000),
    ("zmmul", 1000000),
    ("zca", 1000000),
    ("zcd", 1000000),
    ("zcf", 1000000),
    ("zba", 1000000),
    ("zbb", 1000000),
    ("zbs", 1000000),
    ("svinval", 1000000),
];

/// The stack protector level and the control flow protection mode, with the values gcc 16 gives.
///
/// These are defined for assembly too, as gcc does, because an assembly file that has `endbr64`
/// at each entry and the property note only when `__CET__` says so is the usual way to write one.
fn hardening(d: &mut Defs, target: &TargetInfo, opts: &Predef) {
    match opts.protector {
        Protector::None => {}
        Protector::Buffers => d.set("__SSP__", "1"),
        Protector::All => d.set("__SSP_ALL__", "2"),
        Protector::Strong => d.set("__SSP_STRONG__", "3"),
        Protector::Explicit => d.set("__SSP_EXPLICIT__", "4"),
    }
    // The kernel's `compiler-gcc.h` reads it to decide what `__no_sanitize_address` is.
    d.flag_if(opts.address, "__SANITIZE_ADDRESS__");
    // A set of bits: 1 is the branch half, 2 is the return half and 8 is `check`. gcc defines it
    // only on x86, and the AArch64 form of the question is `-mbranch-protection=`.
    let x86 = matches!(target.tuple.arch(), tuple::Arch::X86_64 | tuple::Arch::X86);
    let bits = match opts.control {
        Control::None => None,
        Control::Branch => Some("1"),
        Control::Return => Some("2"),
        Control::Full => Some("3"),
        Control::Check => Some("8"),
    };
    if let Some(bits) = bits.filter(|_| x86) {
        d.set("__CET__", bits);
    }
}

/// The deployment target, which is the oldest release of the OS the program is promised to run
/// on. `Availability.h` reads it through `__ENVIRONMENT_OS_VERSION_MIN_REQUIRED__` and the older
/// per platform spelling, and turns it into `__MAC_OS_X_VERSION_MIN_REQUIRED`, which the whole
/// SDK tests to decide which declarations exist and which are marked unavailable. Without it
/// every one of those tests sees no version at all.
///
/// The version comes from the tuple, as in `aarch64-macos.13`, where `-mmacosx-version-min=`
/// also puts it. With none given this is 11.0 on macOS, the first release that ran on Apple
/// silicon, and 14.0 on iOS, the oldest a current SDK builds for. The oldest answer is the one
/// that cannot hand a program a declaration the machine it runs on does not have. The encoding
/// is two digits each for the major, minor and patch numbers, so 13.4 is 130400.
fn deployment_target(d: &mut Defs, target: &TargetInfo) {
    let (platform, default) = match target.tuple.os() {
        tuple::Os::MacOs => ("MAC_OS_X", tuple::Version::new(11, 0)),
        tuple::Os::IOs => ("IPHONE_OS", tuple::Version::new(14, 0)),
        _ => return,
    };
    let version = target.tuple.os_version().unwrap_or(default);
    let encoded = version.major_part() * 10000
        + version.minor_part().unwrap_or(0) * 100
        + version.patch_part().unwrap_or(0);
    d.set(&format!("__ENVIRONMENT_{platform}_VERSION_MIN_REQUIRED__"), &encoded.to_string());
    d.set("__ENVIRONMENT_OS_VERSION_MIN_REQUIRED__", &encoded.to_string());
}

/// The five spellings a Windows header writes a calling convention and an attribute in.
///
/// `int __cdecl f(void);` is the second declaration in mingw-w64's `stdio.h` and it stops a parser
/// that has never heard of `__cdecl`, which reads it as the name being declared and then finds a
/// second one. None of the five is a keyword, though: gcc defines every one of them as a macro over
/// the GNU spelling of the same thing, which is why they are keywords on Windows and nowhere else
/// without anything in its lexer being told what the target is. `-dM -E` on a mingw-w64 gcc prints
/// exactly the lines below.
///
/// `__declspec(x)` being the GNU spelling of its argument is the part worth saying out loud, since
/// it means `__declspec(dllimport)` and `__attribute__((dllimport))` cannot come to mean different
/// things: there is one attribute and two ways of writing it, and everything that reads attributes
/// reads both.
///
/// On x86-64 all four conventions name the one convention the target has, so the attributes go
/// where every attribute nothing implements goes, which is left on the declaration. On i386 they
/// differ over who pops the arguments and the choice is written into the symbol name, which is
/// document 06.5 and is that target's work when there is one.
fn windows_spellings(d: &mut Defs, opts: &Predef) {
    for name in ["cdecl", "stdcall", "fastcall", "thiscall"] {
        d.set(&format!("__{name}"), &format!("__attribute__((__{name}__))"));
        // The single underscore spellings are not in the reserved namespace, so an implementation
        // may not take them in a strict ISO mode and gcc does not.
        if opts.gnu_extensions {
            d.set(&format!("_{name}"), &format!("__attribute__((__{name}__))"));
        }
    }
    d.set("__declspec(x)", "__attribute__((x))");
}

/// What an MSVC row says about itself, which is what clang says for `-target
/// x86_64-pc-windows-msvc` and no more.
///
/// The mingw set is the thing to compare against, and most of it is missing on purpose. The
/// `__MINGW*` pair and `__MSVCRT__` name a runtime and a header tree this row does not use, and
/// the SDK headers take the wrong branch when they see them. The unarmoured `WIN32` and the
/// `__WIN32__` family are gcc's, and Microsoft's own compiler defines only `_WIN32`, so a header
/// written for it tests nothing else. The calling conventions and `__declspec` are keywords here
/// rather than the macros [`windows_spellings`] writes, since the SDK headers use `__declspec`
/// items that no GNU attribute spells. `__SEH__` is gcc's and says the exception tables are
/// there, which on this row they are not.
///
/// `_MT` and `_DLL` follow the runtime `-fms-runtime-lib=` chose.
fn msvc(d: &mut Defs, target: &TargetInfo, arch: Arch, opts: &Predef) {
    d.flag("_WIN32");
    if target.pointer_width == 64 {
        d.flag("_WIN64");
    }
    // Microsoft's names for the architecture, with Microsoft's values: `_M_X64` is 100 because
    // `_M_IX86` is a processor generation times one hundred and this was the one after.
    match arch {
        Arch::X86_64 => {
            d.set("_M_X64", "100");
            d.set("_M_AMD64", "100");
        }
        Arch::Aarch64 => d.flag("_M_ARM64"),
        // The same count for 32-bit x86, where 600 is the Pentium Pro and what clang and
        // Microsoft's compiler both say today.
        Arch::X86 => d.set("_M_IX86", "600"),
        Arch::Riscv64 | Arch::Wasm32 => {}
    }
    d.set("_MSC_VER", &opts.msc.msc_ver().to_string());
    d.set("_MSC_FULL_VER", &opts.msc.msc_full_ver().to_string());
    d.flag("_MSC_BUILD");
    d.flag_if(opts.ms_extensions.unwrap_or(true), "_MSC_EXTENSIONS");
    // Which C runtime the program is linked against. `_MT` says the runtime is the thread safe
    // one, which every runtime since Visual Studio 2005 is, so it is defined for both. `_DLL` says
    // it is in a DLL, and it is what makes the headers declare its functions as imported.
    d.flag("_MT");
    d.flag_if(opts.ms_dll_runtime, "_DLL");
    // The widest integer the compiler has, which is what Microsoft's headers ask instead of
    // asking about `long long`.
    d.set("_INTEGRAL_MAX_BITS", "64");
    // Two promises the UCRT headers read: `char16_t` is a type the language has, and the narrow
    // execution character set is UTF-8, which is code page 65001.
    d.flag("_HAS_CHAR16_T_LANGUAGE_SUPPORT");
    d.set("_MSVC_EXECUTION_CHARACTER_SET", "65001");
    // `volatile` means what ISO says it does and not the acquire and release that MSVC gives it
    // on x86 by default, which is `/volatile:iso` and what clang defines this for.
    d.flag("_ISO_VOLATILE");
}

/// `__CHAR_BIT__`, the `__SIZEOF_*__` family and the alignment macros.
fn sizes(d: &mut Defs, target: &TargetInfo, opts: &Predef) {
    let isa = opts.isa;
    let pointer = target.pointer_width / 8;
    // The two hardware interference sizes, which say how far apart two objects have to be for
    // a write to one not to invalidate the other's cache line, and how close together two have
    // to be to share one. A cache line is sixty four bytes on every target here, and x86-64
    // gives that for both. Arm cores have been built with lines up to 256 bytes, so gcc and
    // clang both give 256 for the distance that has to be safe on any of them.
    // Apple's cores are the exception that is known exactly, a 128 byte line, and clang for
    // Apple says 128.
    let destructive = match (target.tuple.arch(), target.tuple.os()) {
        (tuple::Arch::Aarch64, tuple::Os::MacOs | tuple::Os::IOs) => "128",
        (tuple::Arch::Aarch64, _) => "256",
        _ => "64",
    };
    // gcc 12 is the first release that says them, and it says them only for a machine whose
    // description gives a cache line. RISC-V, POWER and IBM Z give none, and gcc 13 on Ubuntu
    // and gcc 14 and 15 on FreeBSD say neither name there.
    let lineless = matches!(
        target.tuple.arch(),
        tuple::Arch::Riscv64 | tuple::Arch::Riscv32 | tuple::Arch::PowerPc64 | tuple::Arch::S390x
    );
    let interference = opts.gnuc.major >= 12 && !lineless;
    d.set_if(interference, "__GCC_CONSTRUCTIVE_SIZE", "64");
    d.set_if(interference, "__GCC_DESTRUCTIVE_SIZE", destructive);
    let long = target.long_width / 8;
    let long_double = target.long_double_width / 8;
    d.set("__CHAR_BIT__", "8");
    d.set("__SIZEOF_SHORT__", "2");
    d.set("__SIZEOF_INT__", "4");
    d.set("__SIZEOF_LONG__", &long.to_string());
    d.set("__SIZEOF_LONG_LONG__", "8");
    // gcc has a 128-bit integer only where a register is 64 bits, and a program that finds this
    // macro takes it as the promise that `__int128` compiles. So i686 does not see it, and wasm32
    // and x32 do, because their values are sixty four bits wide and their pointers are not.
    if target.scalars.has_int128 {
        d.set("__SIZEOF_INT128__", "16");
    }
    d.set("__SIZEOF_FLOAT__", "4");
    d.set("__SIZEOF_DOUBLE__", "8");
    d.set("__SIZEOF_LONG_DOUBLE__", &long_double.to_string());
    // Where the type has its old name and nowhere else, which is gcc's rule and the reason the
    // macro is the one portable code tests before it writes `__float128`. PowerPC also says
    // `__FLOAT128__`, which gcc defines there and not on x86. wasm32 says `__FLOAT128__` as clang
    // does and not the size: code that finds the size includes `quadmath.h`, and wasi-libc has
    // none.
    if target.type_names().iter().any(|&(name, _)| name == "__float128") {
        let wasm = target.tuple.arch() == tuple::Arch::Wasm32;
        d.set_if(!wasm, "__SIZEOF_FLOAT128__", "16");
        if matches!(target.tuple.arch(), tuple::Arch::PowerPc64 | tuple::Arch::Wasm32) {
            d.set("__FLOAT128__", "1");
        }
    }
    d.set("__SIZEOF_POINTER__", &pointer.to_string());
    d.set("__SIZEOF_SIZE_T__", &pointer.to_string());
    d.set("__SIZEOF_PTRDIFF_T__", &pointer.to_string());
    d.set("__SIZEOF_WCHAR_T__", &wchar(target).size.to_string());
    // From the spelling rather than from a constant, because Windows makes `wint_t` a
    // `short unsigned int` and this said four bytes there while `__WINT_WIDTH__` said sixteen
    // bits two hundred lines away.
    d.set("__SIZEOF_WINT_T__", &(wint(target).width / 8).to_string());
    // The alignment of the most aligned scalar, which is `max_align_t`'s. That is sixteen where
    // `long double` is sixteen bytes and eight on Apple arm64, where it is a `double`. What
    // `aligned` with no argument gives is a separate number and is sixteen on every target,
    // clang for Apple included.
    //
    // gcc for x86 counts the vector registers too, so the number is 32 under `-mavx` and 64
    // under `-mavx512f`, and `aligned` with no argument still gives 16. That is true of `-m32` as
    // well. clang keeps 16 whatever the flags say, so Apple and the MSVC targets keep 16 here.
    let has = |name| Feature::named(name).is_some_and(|it| isa.has(it));
    let apple = matches!(target.tuple.os(), tuple::Os::MacOs | tuple::Os::IOs);
    let msvc = target.tuple.env() == tuple::Env::Msvc;
    let x86 = matches!(target.tuple.arch(), tuple::Arch::X86_64 | tuple::Arch::X86);
    let wide = x86 && !apple && !msvc;
    let biggest = match target.tuple.arch() {
        tuple::Arch::Aarch64 if apple => "8",
        _ if wide && has("avx512f") => "64",
        _ if wide && has("avx") => "32",
        _ => "16",
    };
    d.set("__BIGGEST_ALIGNMENT__", biggest);
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
    match target.tuple.os() {
        tuple::Os::Windows => {
            Wint { spelling: "short unsigned int", max: "0xffff", min: "0", width: 16 }
        }
        // Every BSD makes it an `int` too, which is `WINT_TYPE` in gcc's `freebsd.h`, `netbsd.h`,
        // `dragonfly.h` and `i386/openbsdelf.h`.
        os if os.is_darwin() || os.is_bsd() => {
            Wint { spelling: "int", max: "0x7fffffff", min: "(-__WINT_MAX__ - 1)", width: 32 }
        }
        _ => Wint { spelling: "unsigned int", max: "0xffffffffU", min: "0U", width: 32 },
    }
}

/// The integer type names, their limits, and the exact width family.
fn integers(d: &mut Defs, target: &TargetInfo, opts: &Predef) {
    // The one fact everything below turns on: which type is 64 bits wide. On LP64 it is
    // `long`, and on Windows LLP64 it is `long long`, and every `size_t`, `intmax_t` and
    // `int64_t` spelling follows from that.
    let lp64 = target.long_width == 64;
    let wide = if lp64 { "long int" } else { "long long int" };
    let wide_unsigned = if lp64 { "long unsigned int" } else { "long long unsigned int" };
    let wide_suffix = if lp64 { "L" } else { "LL" };
    let wide_max = format!("0x7fffffffffffffff{wide_suffix}");
    let wide_umax = format!("0xffffffffffffffffU{wide_suffix}");
    // Apple is LP64 and still makes `int64_t` a `long long`, which `<sys/_types/_int64_t.h>`
    // writes out by hand. The exact, least and fast sixty four bit types follow it, while
    // `intmax_t`, `intptr_t` and `size_t` stay `long`. Saying `long` here gave a freestanding
    // `stdint.h` an `int64_t` that the SDK's own typedef then redefined as a different type.
    //
    // OpenBSD goes further, and gcc's `openbsd-stdint.h` makes every sixty four bit type a `long
    // long` on every processor, `intmax_t` included. `intptr_t` stays `long`.
    let apple = matches!(target.tuple.os(), tuple::Os::MacOs | tuple::Os::IOs);
    let openbsd = target.tuple.os() == tuple::Os::OpenBsd;
    let long_long = ("long long int", "long long unsigned int", "LL");
    let (int64, uint64, int64_suffix) =
        if apple || openbsd { long_long } else { (wide, wide_unsigned, wide_suffix) };
    let (intmax, uintmax, intmax_suffix) =
        if openbsd { long_long } else { (wide, wide_unsigned, wide_suffix) };
    // The types as wide as a pointer, which are the wide ones above everywhere but a thirty two
    // bit target, where they are `int`. That is what gcc says for i386 on Linux and on Windows,
    // and it is what `rucc_sema` gives `sizeof` and a pointer difference, which have to agree.
    // wasm32 is the thirty two bit target where they are `long`, which wasi-libc's headers say,
    // and `rucc_sema` reads the same field of the layout.
    let (pointer, pointer_unsigned, pointer_max, pointer_umax) =
        if target.scalars.pointer_int_is_long {
            ("long int", "long unsigned int", "0x7fffffffL".to_string(), "0xffffffffUL".to_string())
        } else if target.pointer_width == 32 {
            ("int", "unsigned int", "0x7fffffff".to_string(), "0xffffffffU".to_string())
        } else {
            (wide, wide_unsigned, wide_max.clone(), wide_umax.clone())
        };
    let int64_max = format!("0x7fffffffffffffff{int64_suffix}");
    let int64_umax = format!("0xffffffffffffffffU{int64_suffix}");

    d.set("__SCHAR_MAX__", "0x7f");
    d.set("__SHRT_MAX__", "0x7fff");
    d.set("__INT_MAX__", "0x7fffffff");
    d.set("__LONG_MAX__", if lp64 { "0x7fffffffffffffffL" } else { "0x7fffffffL" });
    d.set("__LONG_LONG_MAX__", "0x7fffffffffffffffLL");
    d.set("__INTMAX_MAX__", &format!("0x7fffffffffffffff{intmax_suffix}"));
    d.set("__UINTMAX_MAX__", &format!("0xffffffffffffffffU{intmax_suffix}"));
    d.set("__SIZE_MAX__", &pointer_umax);
    d.set("__PTRDIFF_MAX__", &pointer_max);
    d.set("__INTPTR_MAX__", &pointer_max);
    d.set("__UINTPTR_MAX__", &pointer_umax);
    // `long` on wasm32, whose `signal.h` says `typedef long sig_atomic_t;`, and `int` everywhere
    // else. That includes i386 OpenBSD, where the pointer sized integer is a `long` as well.
    let wasm32 = target.tuple.arch() == tuple::Arch::Wasm32;
    let sig_atomic = if wasm32 { "long int" } else { "int" };
    d.set("__SIG_ATOMIC_MAX__", if sig_atomic == "int" { "0x7fffffff" } else { "0x7fffffffL" });
    d.set("__SIG_ATOMIC_MIN__", "(-__SIG_ATOMIC_MAX__ - 1)");
    // The widest `_BitInt` this compiler builds, which is narrower than gcc 16's sixty five
    // thousand five hundred and thirty five because a folded constant here is a hundred and
    // twenty eight bits wide. A program that reads this macro to decide what to write gets an
    // answer it can rely on, which is the point of saying a number smaller than gcc's rather
    // than saying gcc's and refusing what it asked for. `MAX_BIT_INT_WIDTH` in `rucc-sema` is
    // the same number and has to be changed with it.
    // gcc 14 is the first release with `_BitInt`, and gcc 14 and 15 have it on x86 and AArch64
    // alone, which is where the `__LIBGCC_BITINT_*` macros below are too. So a persona that
    // claims an older gcc, or a RISC-V gcc 15, does not say the macro, and a header that tests
    // it does not reach for the type. RISC-V under gcc 16 keeps it, as it was before.
    let riscv = target.tuple.arch() == tuple::Arch::Riscv64;
    let bitint = opts.gnuc.major >= 14 && !(riscv && opts.gnuc.major < 16);
    d.set_if(bitint, "__BITINT_MAXWIDTH__", "128");

    let wchar = wchar(target);
    d.set("__WCHAR_TYPE__", wchar.spelling);
    d.set("__WCHAR_MAX__", wchar.max);
    d.set("__WCHAR_MIN__", wchar.min);
    let wint = wint(target);
    d.set("__WINT_TYPE__", wint.spelling);
    d.set("__WINT_MAX__", wint.max);
    d.set("__WINT_MIN__", wint.min);
    d.set("__SIZE_TYPE__", pointer_unsigned);
    d.set("__PTRDIFF_TYPE__", pointer);
    d.set("__INTMAX_TYPE__", intmax);
    d.set("__UINTMAX_TYPE__", uintmax);
    d.set("__INTPTR_TYPE__", pointer);
    d.set("__UINTPTR_TYPE__", pointer_unsigned);
    d.set("__SIG_ATOMIC_TYPE__", sig_atomic);
    d.set("__CHAR16_TYPE__", "short unsigned int");
    d.set("__CHAR32_TYPE__", "unsigned int");
    d.set("__INTMAX_C(c)", &format!("c ## {intmax_suffix}"));
    d.set("__UINTMAX_C(c)", &format!("c ## U{intmax_suffix}"));

    // The exact width family, which is what a freestanding `stdint.h` is written out of.
    exact(d, 8, "signed char", "unsigned char", "0x7f", "0xff", "");
    exact(d, 16, "short int", "short unsigned int", "0x7fff", "0xffff", "");
    // No suffix. An `int` needs none, and the `U` on the unsigned side is added by `exact`
    // rather than being part of the width.
    exact(d, 32, "int", "unsigned int", "0x7fffffff", "0xffffffffU", "");
    exact(d, 64, int64, uint64, &int64_max, &int64_umax, int64_suffix);

    // The fast types. GCC makes the 16 and 32 bit ones `long` on sixty four bit glibc, which
    // is what glibc's `stdint.h` says under `__WORDSIZE == 64` whatever the processor, and
    // `int` everywhere else. A header that computes a printf format from the type name notices
    // the difference. gcc for aarch64, riscv64, powerpc64le and s390x all say `long int`.
    //
    // musl is the reason this is not simply a question of the architecture. musl defines
    // `int_fast16_t` and `int_fast32_t` as `int32_t` on every target it supports, GCC built
    // for a musl target agrees with it, and GCC built for glibc on the same processor does
    // not. The place it shows is `stdatomic.h`, which GCC ships and writes directly out of
    // these macros: `typedef _Atomic __INT_FAST16_TYPE__ atomic_int_fast16_t;`. Get this wrong
    // and every atomic fast type in the program is the wrong width.
    let fast_is_wide =
        target.tuple.os() == tuple::Os::Linux && lp64 && target.tuple.env() != tuple::Env::Musl;
    let fast_middle = if fast_is_wide { wide } else { "int" };
    // Windows is the exception in the other direction, and it is only the 16 bit one. mingw's
    // `stdint.h` makes `int_fast16_t` a `short` and gcc for that target says the same, while
    // `int_fast32_t` there is the `int` it is nearly everywhere, so the two cannot share an
    // answer on this platform the way they do on the others. The measurement is the mingw tree,
    // which is the only Windows header tree this compiler fetches, and the msvc environment is
    // given the same answer because nothing compiles against a tree of Microsoft's yet. Apple's
    // `stdint.h` is the same shape, `int16_t` for the 16 bit one and `int32_t` for the other.
    let fast16_is_short =
        matches!(target.tuple.os(), tuple::Os::Windows | tuple::Os::MacOs | tuple::Os::IOs);
    // The BSDs make the 8 bit one an `int` as well, which is what gcc's `freebsd-stdint.h`,
    // `netbsd-stdint.h`, `openbsd-stdint.h` and `dragonfly-stdint.h` say on every processor here.
    let fast8_is_int = target.tuple.os().is_bsd();
    if fast8_is_int {
        d.set("__INT_FAST8_TYPE__", "int");
        d.set("__UINT_FAST8_TYPE__", "unsigned int");
        d.set("__INT_FAST8_MAX__", "0x7fffffff");
        d.set("__UINT_FAST8_MAX__", "0xffffffffU");
    } else {
        d.set("__INT_FAST8_TYPE__", "signed char");
        d.set("__UINT_FAST8_TYPE__", "unsigned char");
        d.set("__INT_FAST8_MAX__", "0x7f");
        d.set("__UINT_FAST8_MAX__", "0xff");
    }
    for width in [16, 32] {
        let (signed, unsigned, max, umax) = if width == 16 && fast16_is_short {
            ("short int", "short unsigned int", "0x7fff", "0xffff")
        } else if fast_middle == "int" {
            ("int", "unsigned int", "0x7fffffff", "0xffffffffU")
        } else {
            (wide, wide_unsigned, wide_max.as_str(), wide_umax.as_str())
        };
        d.set(&format!("__INT_FAST{width}_TYPE__"), signed);
        d.set(&format!("__UINT_FAST{width}_TYPE__"), unsigned);
        d.set(&format!("__INT_FAST{width}_MAX__"), max);
        d.set(&format!("__UINT_FAST{width}_MAX__"), umax);
    }
    d.set("__INT_FAST64_TYPE__", int64);
    d.set("__UINT_FAST64_TYPE__", uint64);
    d.set("__INT_FAST64_MAX__", &int64_max);
    d.set("__UINT_FAST64_MAX__", &int64_umax);

    let fast32 = if fast_is_wide { 64 } else { 32 };
    let fast16 = if fast16_is_short { 16 } else { fast32 };
    let fast8 = if fast8_is_int { 32 } else { 8 };
    widths(d, target, &wchar, &wint, [fast8, fast16, fast32]);
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
fn widths(d: &mut Defs, target: &TargetInfo, wchar: &Wchar, wint: &Wint, fast: [u32; 3]) {
    let [fast8, fast16, fast32] = fast;
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
    d.set("__INT_FAST8_WIDTH__", &fast8.to_string());
    d.set("__INT_FAST16_WIDTH__", &fast16.to_string());
    d.set("__INT_FAST32_WIDTH__", &fast32.to_string());
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
    /// The largest value with a full significand, which is `max` for every format whose values
    /// all have one and is smaller for the double-double, whose largest values do not.
    ///
    /// A double-double's high half can be as large as a `double` gets while its low half is
    /// nowhere near, and the sum is then a number above anything the format can write with a
    /// hundred and six significand bits behind it. So `LDBL_MAX` on PowerPC is `DBL_MAX` and
    /// `LDBL_NORM_MAX` is a bit under half of it, and a program that reaches for the largest
    /// value it can compute with wants the second.
    norm_max: &'static str,
    min: &'static str,
    epsilon: &'static str,
    denorm_min: &'static str,
    /// Whether the format is one IEC 60559 describes, which every one of them is but the brain
    /// float, whose significand is a `float`'s with sixteen bits cut off the end of it, and the
    /// double-double, which is not a binary floating point format in IEC 60559's sense at all.
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
    norm_max: "6.55040000000000000000000000000000000e+4",
    min: "6.10351562500000000000000000000000000e-5",
    epsilon: "9.76562500000000000000000000000000000e-4",
    denorm_min: "5.96046447753906250000000000000000000e-8",
    is_iec_60559: "1",
};

/// The brain float, which is `__bf16`.
const BFLOAT16: Characteristics = Characteristics {
    mant_dig: "8",
    dig: "2",
    min_exp: "(-125)",
    min_10_exp: "(-37)",
    max_exp: "128",
    max_10_exp: "38",
    decimal_dig: "4",
    max: "3.38953138925153547590470800371487867e+38",
    norm_max: "3.38953138925153547590470800371487867e+38",
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
    norm_max: "3.40282346638528859811704183484516925e+38",
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
    norm_max: "1.79769313486231570814527423731704357e+308",
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
    norm_max: "1.18973149535723176502126385303097021e+4932",
    min: "3.36210314311209350626267781732175260e-4932",
    epsilon: "1.08420217248550443400745280086994171e-19",
    denorm_min: "3.64519953188247460252840593361941982e-4951",
    is_iec_60559: "1",
};

/// The x87 format rounded to fifty three bits, which is `long double` and `_Float64x` on i386
/// FreeBSD. It is GCC's `ieee_extended_intel_96_round_53_format`: the precision of a `double`
/// and the exponent range of the x87 format.
///
/// The values are what `builtin_define_float_constants` in GCC's `c-cppbuiltin.cc` makes of
/// that format. `MAX` is `(1 - 2^-53) * 2^16384`, `MIN` is `2^-16382` as on the x87 row,
/// `EPSILON` is `2^-52` and `DENORM_MIN` is `2^(emin - p)`, which is `2^-16434`. Each is written
/// to thirty six digits, rounded to nearest with ties to even, because GCC writes every value to
/// the digits of the widest format, and that is `_Float128` on i386.
const X87_ROUND_53: Characteristics = Characteristics {
    mant_dig: "53",
    dig: "15",
    min_exp: "(-16381)",
    min_10_exp: "(-4931)",
    max_exp: "16384",
    max_10_exp: "4932",
    decimal_dig: "17",
    max: "1.18973149535723163299902939989638351e+4932",
    norm_max: "1.18973149535723163299902939989638351e+4932",
    min: "3.36210314311209350626267781732175260e-4932",
    epsilon: "2.22044604925031308084726333618164062e-16",
    denorm_min: "7.46536864129530798597817535205257178e-4948",
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
    norm_max: "1.18973149535723176508575932662800702e+4932",
    min: "3.36210314311209350626267781732175260e-4932",
    epsilon: "1.92592994438723585305597794258492732e-34",
    denorm_min: "6.47517511943802511092443895822764655e-4966",
    is_iec_60559: "1",
};

/// IBM double-double, which is `long double` on 64-bit PowerPC.
///
/// The row that does not follow from a precision and an exponent range, because the format has
/// neither. `MANT_DIG` is 106 and `DIG` is 31, which are the figures near the top of the
/// significand and not everywhere. `MAX` is a little above `DBL_MAX`, since the high half can be
/// `DBL_MAX` and the low half then adds to it, and `NORM_MAX` is about half of that, so this is
/// the one row where the two are different numbers. `EPSILON` is the same number as `DENORM_MIN`,
/// two to the minus one thousand and seventy four, because the smallest value that changes a
/// double-double near one is a subnormal in the low half rather than one unit in the last place
/// of anything. `MIN` is two to the minus nine hundred and sixty nine rather than `DBL_MIN`,
/// because below that the low half has no room left to be normal in.
///
/// Every value here is what the reference compiler prints for `powerpc64le-linux-gnu`, checked
/// rather than derived, on the same terms as the data layouts in `rucc-abi`. The format is one
/// where deriving them is how the four wrong numbers in those layouts happened.
const DOUBLE_DOUBLE: Characteristics = Characteristics {
    mant_dig: "106",
    dig: "31",
    min_exp: "(-968)",
    min_10_exp: "(-291)",
    max_exp: "1024",
    max_10_exp: "308",
    decimal_dig: "33",
    max: "1.79769313486231580793728971405301e+308",
    norm_max: "8.98846567431157953864652595394501e+307",
    min: "2.00416836000897277799610805135016e-292",
    epsilon: "4.94065645841246544176568792868221e-324",
    denorm_min: "4.94065645841246544176568792868221e-324",
    is_iec_60559: "0",
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
        Format::X87Round53 => &X87_ROUND_53,
        Format::Quad => &QUAD,
        Format::DoubleDouble => &DOUBLE_DOUBLE,
        // No target has a decimal `long double` or `_Float64x`, which are the two rows a target
        // chooses, and the decimal types have `__DEC*` macros of their own rather than a row here.
        Format::Decimal32 | Format::Decimal64 | Format::Decimal128 => {
            panic!("a binary type's format is never a decimal one")
        }
    }
}

/// The `float.h` characteristics.
///
/// Nine families of them, which is `float`, `double` and `long double` and the six C23 named
/// them after. Two of the nine have a format the target decides: `long double`, which is x87 on
/// x86-64 Linux, quad on AArch64 and RISC-V Linux and a `double` on Apple and on Windows, and
/// `_Float64x`, which is the widest format the processor has and so does not follow `long
/// double` down on the targets that shrink it.
///
/// Three more have a target that decides whether they are there at all. `_Float64x` is missing
/// on a machine with nothing wider than a `double`, and `_Float16` and `_Float128` are missing
/// wherever the machine has no such format, which is four of the seven rows for the half and one
/// of them for the quad.
///
/// `__FLT128X_*__` is deliberately missing. `_Float128x` is a type no target gcc supports has,
/// so gcc defines nothing for it and neither does this.
/// Whether the unit may use a float unit at all, which gcc needs before it says IEC 60559. The
/// x86 kernels take away both the x87 and SSE, the AArch64 kernels the FP registers, and a RISC-V
/// machine with no F extension has none.
fn float_unit(target: &TargetInfo, opts: &Predef) -> bool {
    match target.tuple.arch() {
        tuple::Arch::X86_64 | tuple::Arch::X86 => {
            opts.x87 || Feature::named("sse").is_some_and(|it| opts.isa.has(it))
        }
        tuple::Arch::Aarch64 => opts.vector,
        tuple::Arch::Riscv64 | tuple::Arch::Riscv32 => {
            RiscvIsa::new(opts.riscv_arch.as_deref(), opts.gnuc.major).has("f")
        }
        _ => true,
    }
}

fn floats(d: &mut Defs, target: &TargetInfo, opts: &Predef) {
    d.set("__FLT_RADIX__", "2");
    // Real arithmetic follows IEC 60559 in every format on every target here, which is what the
    // value two says. The complex one beside it says the same about complex arithmetic, and gcc
    // gives both the value two on every target this compiler has. These two are read rather than
    // tested: glibc's `<stdc-predef.h>` asks what the compiler intended and writes the
    // `__STDC_IEC_559` family from the answer, and a compiler that says nothing is presumed to
    // have meant yes. So the choice is not between claiming and not claiming, it is between
    // saying so and having it said for us. Zero for both once a fast math licence is given,
    // which is gcc's answer and what keeps glibc from claiming the family on our behalf.
    // Zero on wasm32 as well, which is the value a program reads from clang, which does not define
    // the two names. A positive value there would make a header claim Annex F for a library that
    // does not give it.
    let wasm = target.tuple.arch() == tuple::Arch::Wasm32;
    let gnuc = opts.gnuc.major;
    let iec = opts.math.iec_559(opts.trapping_math) && !wasm && float_unit(target, opts);
    let iec = if iec { "2" } else { "0" };
    d.set("__GCC_IEC_559", iec);
    d.set("__GCC_IEC_559_COMPLEX", iec);
    // Every operation is done in the type of its operands, which is what SSE2 and the AArch64
    // and RISC-V floating units all do. The other two names are the same answer asked under the
    // rules of C99 and of TS 18661-3, which are the same rules for a target with no excess
    // precision to have, and glibc's `<math.h>` reads the last of the three.
    // gcc says the C99 one on AArch64 alone, since only that backend defines it, and gcc 13 on
    // Ubuntu and gcc 10 to 15 on the BSDs leave it out on x86-64 and RISC-V.
    // The answer is 2 where the arithmetic is on the x87 stack, because there a `float` or a
    // `double` expression is evaluated in `long double`, and gcc says 2 for both names there in
    // every dialect and under `-ffast-math` as well. A header reads it to make `float_t` and
    // `double_t` a `long double`.
    let eval = if opts.x87_floats { "2" } else { "0" };
    d.set("__FLT_EVAL_METHOD__", eval);
    d.set_if(target.tuple.arch() == tuple::Arch::Aarch64, "__FLT_EVAL_METHOD_C99__", "0");
    d.set("__FLT_EVAL_METHOD_TS_18661_3__", eval);
    // How a decimal floating expression is evaluated, which gcc says on every target, whether or
    // not the target has the decimal types. It is 2 in gcc, and a program that reads it is asking
    // what gcc would do. clang does not say it, so the rows whose reference is clang leave it out.
    let apple = matches!(target.tuple.os(), tuple::Os::MacOs | tuple::Os::IOs);
    let msvc = target.tuple.env() == tuple::Env::Msvc && !opts.gnuc_given;
    let gcc = !wasm && !apple && !msvc;
    d.set_if(gcc || target.has_decimal_float, "__DEC_EVAL_METHOD__", "2");

    family(d, gnuc, "FLT", &SINGLE, |value| format!("{value}F"));
    // gcc writes the `double` values as `long double` constants cast back down, which is exact
    // in every format `long double` has and is the one family whose values are not a suffix.
    family(d, gnuc, "DBL", &DOUBLE, |value| format!("((double){value}L)"));
    let long_double = characteristics(target.long_double_format);
    family(d, gnuc, "LDBL", long_double, |value| format!("{value}L"));

    // The two named types that are not on every machine, each written where the type is and left
    // out where it is not. A program reads `__FLT128_MANT_DIG__` to find out whether it may write
    // the type, which is what glibc's `<float.h>` and `<math.h>` do, so the macros and the type
    // have to agree or the header asks for something the compiler will refuse.
    // `_Float16` came to gcc on AArch64 first, then to x86 in gcc 12 and to RISC-V in gcc 13, so
    // a persona that claims an older gcc leaves the family out there.
    // gcc 12 has it on x86 only with SSE2, and gcc 13 has it without.
    let x86 = matches!(target.tuple.arch(), tuple::Arch::X86_64 | tuple::Arch::X86);
    let sse2 = Feature::named("sse2").is_some_and(|it| opts.isa.has(it));
    let half_since = match target.tuple.arch() {
        tuple::Arch::X86_64 | tuple::Arch::X86 => 12,
        tuple::Arch::Riscv64 | tuple::Arch::Riscv32 => 13,
        _ => 0,
    };
    let half_since = if x86 && !sse2 { half_since.max(13) } else { half_since };
    if target.has_float16 && gnuc >= half_since {
        family(d, gnuc, "FLT16", &HALF, |value| format!("{value}F16"));
    }
    // `__bf16` became an arithmetic type in gcc 13 on x86 and AArch64, and the family came with it.
    // It is written where the type is a name rucc knows.
    let brain = target.type_names().iter().any(|&(name, _)| name == "__bf16");
    if brain && gnuc >= 13 {
        family(d, gnuc, "BFLT16", &BFLOAT16, |value| format!("{value}BF16"));
    }
    family(d, gnuc, "FLT32", &SINGLE, |value| format!("{value}F32"));
    family(d, gnuc, "FLT64", &DOUBLE, |value| format!("{value}F64"));
    if target.has_float128 {
        family(d, gnuc, "FLT128", &QUAD, |value| format!("{value}F128"));
    }
    family(d, gnuc, "FLT32X", &DOUBLE, |value| format!("{value}F32x"));
    // Nothing at all on a target whose widest format is a `double`, which is what gcc does
    // there: `_Float64x` is not a type on that machine and the family that describes it is not a
    // set of macros with a smaller answer in them.
    if let Some(format) = target.float64x_format {
        family(d, gnuc, "FLT64X", characteristics(format), |value| format!("{value}F64x"));
    }

    // The number itself rather than the name of the other macro. The value is the same either
    // way, since `long double` is the widest format here, but the two are not the same thing to
    // read: `-dM` prints what the macro is, and a program that undefines `__LDBL_DECIMAL_DIG__`
    // takes this one with it. gcc writes the number.
    d.set("__DECIMAL_DIG__", long_double.decimal_dig);
    // gcc has the decimal types on AArch64 from gcc 14, so a persona older than that says none
    // of their macros there.
    let aarch64 = target.tuple.arch() == tuple::Arch::Aarch64;
    if target.has_decimal_float && !(aarch64 && gnuc < 14) {
        decimals(d, gnuc);
    }
}

/// The `__LIBGCC_*__` macros, which gcc defines under `-fbuilding-libgcc` for its own runtime
/// library and nothing else reads. `libgcc2.c`, `crtstuff.c` and the unwinder take the machine's
/// floating point modes, its sections and its unwind registers from them. NetBSD builds its copy of
/// libgcc with the flag.
///
/// The values are the ones gcc 15 gives for each machine on GNU/Linux. A BSD is the same machine
/// and the same ELF, so it gets the same values.
///
/// gcc writes a group of macros for each floating mode the machine has. `MANT_DIG` comes for
/// every mode, and the rest only for a mode the compiler has a type for, named after that type's
/// `float.h` family. `BF` stops at `MANT_DIG` here. gcc 13 and later may name the `__BFLT16_*__`
/// family for it where `__bf16` is a type, and no reference row measures that yet.
fn libgcc(d: &mut Defs, target: &TargetInfo) {
    let Some(triple) = Triple::from_tuple(target.tuple) else {
        return;
    };
    let x86 = matches!(triple.arch, Arch::X86 | Arch::X86_64);
    let long_double = target.long_double_format;
    // The mode, its precision, and the `float.h` family and constant suffix of the type it is, when
    // there is one.
    let mut modes =
        vec![("BF", 8, None), ("HF", 11, target.has_float16.then_some(("FLT16", "f16")))];
    modes.push(("SF", 24, Some(("FLT", "f"))));
    modes.push(("DF", 53, Some(("DBL", ""))));
    if x86 {
        let xf = if long_double.is_x87() {
            Some(("LDBL", "l"))
        } else {
            target.float64x_format.is_some_and(Format::is_x87).then_some(("FLT64X", "f64x"))
        };
        modes.push(("XF", 64, xf));
    }
    let tf = if long_double == Format::Quad {
        Some(("LDBL", "l"))
    } else {
        target.has_float128.then_some(("FLT128", "f128"))
    };
    modes.push(("TF", 113, tf));
    for (mode, digits, named) in modes {
        d.set(&format!("__LIBGCC_{mode}_MANT_DIG__"), &digits.to_string());
        let Some((family, suffix)) = named else { continue };
        d.flag(&format!("__LIBGCC_HAS_{mode}_MODE__"));
        d.set(&format!("__LIBGCC_{mode}_FUNC_EXT__"), suffix);
        // Only the half precision mode is computed wider than it is, and only on x86, where
        // arithmetic on a `_Float16` is done in `float`.
        let excess = x86 && mode == "HF";
        d.set(&format!("__LIBGCC_{mode}_EXCESS_PRECISION__"), if excess { "1" } else { "0" });
        for limit in ["EPSILON", "MAX", "MIN"] {
            d.set(&format!("__LIBGCC_{mode}_{limit}__"), &format!("__{family}_{limit}__"));
        }
    }

    if triple.object_format() == ObjectFormat::Elf {
        d.set("__LIBGCC_EH_TABLES_CAN_BE_READ_ONLY__", "1");
        d.set("__LIBGCC_EH_FRAME_SECTION_NAME__", "\".eh_frame\"");
        if triple.arch == Arch::Aarch64 {
            d.set(
                "__LIBGCC_CTORS_SECTION_ASM_OP__",
                "\"\\t.section\\t.init_array,\\\"aw\\\",%init_array\"",
            );
            d.set(
                "__LIBGCC_DTORS_SECTION_ASM_OP__",
                "\"\\t.section\\t.fini_array,\\\"aw\\\",%fini_array\"",
            );
        }
        d.set("__LIBGCC_TEXT_SECTION_ASM_OP__", "\"\\t.text\"");
        d.flag("__LIBGCC_INIT_ARRAY_SECTION_ASM_OP__");
    }
    // What the unwinder and the trampolines need: the size of a trampoline, the stack pointer's
    // and the return address column's numbers, how many registers a frame describes, and the
    // factor every offset in a CIE is a multiple of.
    let (trampoline, sp, registers, alignment, alternate) = match triple.arch {
        Arch::X86_64 => (28, 7, 17, -8, None),
        Arch::X86 => (14, 7, 17, -4, None),
        Arch::Aarch64 => (40, 31, 97, -8, Some(96)),
        Arch::Riscv64 => (144, 2, 129, -4, Some(64)),
        _ => return,
    };
    d.set("__LIBGCC_TRAMPOLINE_SIZE__", &trampoline.to_string());
    d.flag("__LIBGCC_STACK_GROWS_DOWNWARD__");
    if let Some(column) = alternate {
        d.set("__LIBGCC_DWARF_ALT_FRAME_RETURN_COLUMN__", &column.to_string());
    }
    d.set("__LIBGCC_DWARF_FRAME_REGISTERS__", &registers.to_string());
    d.set("__LIBGCC_DWARF_CIE_DATA_ALIGNMENT__", &alignment.to_string());
    d.flag("__LIBGCC_EH_RETURN_STACKADJ_RTX__");
    d.set("__LIBGCC_STACK_POINTER_REGNUM__", &sp.to_string());
    d.set("__LIBGCC_VTABLE_USES_DESCRIPTORS__", "0");
    d.set("__LIBGCC_HAVE_LIBATOMIC", "0");
    d.flag("__LIBGCC_HAVE_HWDBL__");
    // `_BitInt` is in limbs of a machine word, low limb first, on the three machines gcc gives it.
    if triple.arch != Arch::Riscv64 {
        let limb = if triple.arch == Arch::X86 { "32" } else { "64" };
        d.set("__LIBGCC_BITINT_LIMB_WIDTH__", limb);
        d.set("__LIBGCC_BITINT_ORDER__", "__ORDER_LITTLE_ENDIAN__");
    }
}

/// The limits of the decimal types, on the targets that have them.
///
/// Written out rather than worked out, because each is a fact of the interchange format and the
/// rows are all there will ever be. The spellings are gcc's, down to the parentheses around a
/// negative exponent and the suffix on every constant. `_Decimal64x` is from gcc 15, and it is
/// the decimal128 format with a suffix of its own, so its row is the `_Decimal128` row with
/// `D64x`. `__DEC_EVAL_METHOD__` is not here, because gcc says it on every target.
/// `__DECIMAL_BID_FORMAT__` says the encoding is the binary integer one, which it is on every
/// row that has the types.
fn decimals(d: &mut Defs, gnuc: u32) {
    d.set("__DECIMAL_BID_FORMAT__", "1");
    let rows = [
        ("32", "DF", "7", "(-94)", "97", "1E-95", "9.999999E96", "1E-6", "0.000001E-95"),
        (
            "64",
            "DD",
            "16",
            "(-382)",
            "385",
            "1E-383",
            "9.999999999999999E384",
            "1E-15",
            "0.000000000000001E-383",
        ),
        (
            "128",
            "DL",
            "34",
            "(-6142)",
            "6145",
            "1E-6143",
            "9.999999999999999999999999999999999E6144",
            "1E-33",
            "0.000000000000000000000000000000001E-6143",
        ),
    ];
    let extended = (gnuc >= 15).then(|| {
        let mut row = rows[2];
        row.0 = "64X";
        row.1 = "D64x";
        row
    });
    for (bits, suffix, mant_dig, min_exp, max_exp, min, max, epsilon, subnormal_min) in
        rows.into_iter().chain(extended)
    {
        d.set(&format!("__DEC{bits}_MANT_DIG__"), mant_dig);
        d.set(&format!("__DEC{bits}_MIN_EXP__"), min_exp);
        d.set(&format!("__DEC{bits}_MAX_EXP__"), max_exp);
        d.set(&format!("__DEC{bits}_MIN__"), &format!("{min}{suffix}"));
        d.set(&format!("__DEC{bits}_MAX__"), &format!("{max}{suffix}"));
        d.set(&format!("__DEC{bits}_EPSILON__"), &format!("{epsilon}{suffix}"));
        d.set(&format!("__DEC{bits}_SUBNORMAL_MIN__"), &format!("{subnormal_min}{suffix}"));
    }
}

/// One family of `float.h` macros, named `__{prefix}_*__`.
///
/// `write` turns a value into the constant its macro expands to, which is a suffix for every
/// family but `double`.
///
/// `__{prefix}_IS_IEC_60559__` depends on the gcc release as well. gcc 10 does not say it, and
/// gcc 11 and 12 say 2 where gcc 13 and later say 1.
fn family(
    d: &mut Defs,
    gnuc: u32,
    prefix: &str,
    c: &Characteristics,
    write: impl Fn(&str) -> String,
) {
    d.set(&format!("__{prefix}_MANT_DIG__"), c.mant_dig);
    d.set(&format!("__{prefix}_DIG__"), c.dig);
    d.set(&format!("__{prefix}_MIN_EXP__"), c.min_exp);
    d.set(&format!("__{prefix}_MIN_10_EXP__"), c.min_10_exp);
    d.set(&format!("__{prefix}_MAX_EXP__"), c.max_exp);
    d.set(&format!("__{prefix}_MAX_10_EXP__"), c.max_10_exp);
    d.set(&format!("__{prefix}_DECIMAL_DIG__"), c.decimal_dig);
    d.set(&format!("__{prefix}_MAX__"), &write(c.max));
    d.set(&format!("__{prefix}_NORM_MAX__"), &write(c.norm_max));
    d.set(&format!("__{prefix}_MIN__"), &write(c.min));
    d.set(&format!("__{prefix}_EPSILON__"), &write(c.epsilon));
    d.set(&format!("__{prefix}_DENORM_MIN__"), &write(c.denorm_min));
    let iec = match (gnuc, c.is_iec_60559) {
        (0..=10, _) => None,
        (11 | 12, "1") => Some("2"),
        (_, value) => Some(value),
    };
    if let Some(iec) = iec {
        d.set(&format!("__{prefix}_IS_IEC_60559__"), iec);
    }
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

    /// The same, for a machine the three field triple cannot spell.
    fn set_for_tuple(tuple: &str) -> String {
        let target = TargetInfo::for_tuple(tuple.parse().expect("a row in the target table"));
        built_in(&target, &Predef::new())
    }

    fn has(text: &str, line: &str) -> bool {
        text.lines().any(|l| l == line)
    }

    #[test]
    fn the_version_macros_say_the_version_this_compiler_was_built_at() {
        // They said 0.1.0 through sixty seven releases, because the number was written here as
        // well as in the manifest, so a program asking which rucc it was is now told.
        let text = set_for("x86_64-unknown-linux-gnu");
        assert!(has(&text, &format!("#define __rucc_version__ \"{VERSION}\"")), "{text}");
        assert!(has(&text, &format!("#define __VERSION__ \"rucc {VERSION}\"")), "{text}");
        assert!(has(&text, &format!("#define __rucc_major__ {}", field(VERSION, 0))));
        assert!(has(&text, &format!("#define __rucc_minor__ {}", field(VERSION, 1))));
        assert!(has(&text, &format!("#define __rucc_patchlevel__ {}", field(VERSION, 2))));

        // And what each of them expands to is something an `#if` can read, which is the whole
        // reason the three numbers are separate macros from the string.
        for n in 0..3 {
            assert!(field(VERSION, n).parse::<u32>().is_ok(), "{}", field(VERSION, n));
        }
    }

    #[test]
    fn the_riscv_kernel_has_the_soft_float_abi_and_the_medany_model() {
        let triple: Triple = "riscv64-linux-gnu".parse().expect("riscv64");
        let target = TargetInfo::new(triple);
        let absolute = Predef { pic: Pic::Absolute, ..Predef::new() };
        let hosted = built_in(&target, &absolute);
        for line in [
            "#define __riscv_flen 64",
            "#define __riscv_float_abi_double 1",
            "#define __riscv_cmodel_medlow 1",
        ] {
            assert!(has(&hosted, line), "{line}");
        }
        // The kernel passes `-mabi=lp64` and keeps the machine, which still has the D extension.
        let kernel =
            Predef { vector: false, soft_float: true, code_model: CodeModel::Medany, ..absolute };
        let kernel = built_in(&target, &kernel);
        assert!(has(&kernel, "#define __riscv_float_abi_soft 1"), "{kernel}");
        assert!(has(&kernel, "#define __riscv_cmodel_medany 1"), "{kernel}");
        assert!(has(&kernel, "#define __riscv_flen 64"), "{kernel}");
        assert!(!kernel.contains("__riscv_float_abi_double") && !kernel.contains("cmodel_medlow"));
    }

    /// tamnd/rucc#4088. The RISC-V macros of each gcc release, from the bsd-predef reference of
    /// gcc 10, 12, 14 and 15.
    #[test]
    fn the_riscv_extension_macros_follow_the_gcc_release() {
        let claiming = |triple: &str, major: u32| {
            let triple: Triple = triple.parse().expect("a triple");
            let gnuc = GnucVersion { major, minor: 0, patch: 0 };
            let opts = Predef { gnuc, pic: Pic::Absolute, ..Predef::new() };
            built_in(&TargetInfo::new(triple), &opts)
        };
        let base = [
            "__riscv_div 1",
            "__riscv_mul 1",
            "__riscv_muldiv 1",
            "__riscv_fdiv 1",
            "__riscv_fsqrt 1",
            "__riscv_atomic 1",
            "__riscv_compressed 1",
            "__riscv_flen 64",
            "__FP_FAST_FMA 1",
            "__FP_FAST_FMAF 1",
            "__FP_FAST_FMAF32 1",
            "__FP_FAST_FMAF32x 1",
            "__FP_FAST_FMAF64 1",
        ];
        let named = [
            "__riscv_arch_test 1",
            "__riscv_i 2001000",
            "__riscv_m 2000000",
            "__riscv_a 2001000",
            "__riscv_f 2002000",
            "__riscv_d 2002000",
            "__riscv_c 2000000",
            "__riscv_zicsr 2000000",
            "__riscv_zifencei 2000000",
        ];
        let parts = [
            "__riscv_zaamo 1000000",
            "__riscv_zalrsc 1000000",
            "__riscv_zca 1000000",
            "__riscv_zcd 1000000",
            "__riscv_zmmul 1000000",
        ];
        let narrow = [
            "__GCC_ATOMIC_BOOL_LOCK_FREE",
            "__GCC_ATOMIC_CHAR_LOCK_FREE",
            "__GCC_ATOMIC_CHAR16_T_LOCK_FREE",
            "__GCC_ATOMIC_SHORT_LOCK_FREE",
        ];
        let gcc10 = claiming("riscv64-unknown-netbsd", 10);
        for line in base {
            assert!(has(&gcc10, &format!("#define {line}")), "{line}");
        }
        for line in named.iter().chain(&parts).chain(&["__riscv_misaligned_slow 1"]) {
            assert!(!has(&gcc10, &format!("#define {line}")), "{line}");
        }
        for name in narrow {
            assert!(has(&gcc10, &format!("#define {name} 1")), "{name}");
        }
        assert!(!gcc10.contains("__GCC_HAVE_SYNC_COMPARE_AND_SWAP_1 "));
        assert!(!gcc10.contains("__GCC_HAVE_SYNC_COMPARE_AND_SWAP_2 "));
        assert!(has(&gcc10, "#define __GCC_HAVE_SYNC_COMPARE_AND_SWAP_4 1"));

        let gcc12 = claiming("riscv64-unknown-netbsd", 12);
        for line in base.iter().chain(&named) {
            assert!(has(&gcc12, &format!("#define {line}")), "{line}");
        }
        assert!(!gcc12.contains("__riscv_misaligned_slow"));

        let gcc14 = claiming("riscv64-unknown-freebsd", 14);
        for line in base.iter().chain(&named).chain(&["__riscv_misaligned_slow 1"]) {
            assert!(has(&gcc14, &format!("#define {line}")), "{line}");
        }
        for line in parts {
            assert!(!has(&gcc14, &format!("#define {line}")), "{line}");
        }
        for name in narrow {
            assert!(has(&gcc14, &format!("#define {name} 2")), "{name}");
        }
        assert!(has(&gcc14, "#define __GCC_HAVE_SYNC_COMPARE_AND_SWAP_1 1"));

        let gcc15 = claiming("riscv64-unknown-freebsd", 15);
        for line in base.iter().chain(&named).chain(&parts) {
            assert!(has(&gcc15, &format!("#define {line}")), "{line}");
        }
        // Other machines keep the narrow atomics whatever the release.
        let arm = claiming("aarch64-unknown-netbsd", 10);
        assert!(has(&arm, "#define __GCC_ATOMIC_CHAR_LOCK_FREE 2"), "{arm}");
    }

    /// tamnd/rucc#4088 and tamnd/rucc#4096. The extensions come from `-march=`, as the FreeBSD
    /// kernel and the `lp64d` row of the reference write it.
    #[test]
    fn the_riscv_extension_macros_follow_the_march() {
        let built = |march: &str, soft_float: bool| {
            let triple: Triple = "riscv64-unknown-freebsd".parse().expect("a triple");
            let gnuc = GnucVersion { major: 15, minor: 0, patch: 0 };
            let opts =
                Predef { gnuc, soft_float, riscv_arch: Some(march.to_owned()), ..Predef::new() };
            built_in(&TargetInfo::new(triple), &opts)
        };
        let kernel = built("rv64imafdch_zifencei_svinval", true);
        for line in [
            "__riscv_h 1000000",
            "__riscv_svinval 1000000",
            "__riscv_zifencei 2000000",
            "__riscv_zicsr 2000000",
            "__riscv_flen 64",
            "__riscv_float_abi_soft 1",
            "__FP_FAST_FMA 1",
        ] {
            assert!(has(&kernel, &format!("#define {line}")), "{line}");
        }
        // gcc reads `imafdc` as it is written, with no `zifencei`.
        let lp64d = built("rv64imafdc", false);
        assert!(has(&lp64d, "#define __riscv_zicsr 2000000"), "{lp64d}");
        assert!(!lp64d.contains("__riscv_zifencei"), "{lp64d}");
        // No float registers, no compressed instructions, and versions written in the string.
        let imac = built("rv64i2p1m2p0a2p1c_zicsr2p0", true);
        assert!(has(&imac, "#define __riscv_i 2001000"), "{imac}");
        assert!(has(&imac, "#define __riscv_zicsr 2000000"), "{imac}");
        assert!(has(&imac, "#define __riscv_compressed 1"), "{imac}");
        for name in ["__riscv_flen", "__riscv_fdiv", "__FP_FAST_FMA", "__riscv_d ", "__riscv_f "] {
            assert!(!imac.contains(name), "{name}");
        }
        let single = built("rv64imafc", true);
        assert!(has(&single, "#define __riscv_flen 32"), "{single}");
        assert!(has(&single, "#define __FP_FAST_FMAF 1"), "{single}");
        assert!(!single.contains("#define __FP_FAST_FMA 1"), "{single}");
        assert_eq!(without_version("zicsr2p0"), "zicsr");
        assert_eq!(without_version("zicsr2"), "zicsr");
        assert_eq!(without_version("zbkb"), "zbkb");
    }

    /// tamnd/rucc#4088. Position independent code is its own code model to gcc. gcc 10 says `pic`,
    /// gcc 12 says `pic` beside `medany`, and gcc 13 and later say `medany` alone.
    #[test]
    fn position_independent_riscv_code_is_the_medany_model() {
        let built = |major: u32, pic: Pic| {
            let triple: Triple = "riscv64-unknown-freebsd".parse().expect("a triple");
            let gnuc = GnucVersion { major, minor: 0, patch: 0 };
            built_in(&TargetInfo::new(triple), &Predef { gnuc, pic, ..Predef::new() })
        };
        for pic in [Pic::Library, Pic::Executable] {
            let gcc15 = built(15, pic);
            assert!(has(&gcc15, "#define __riscv_cmodel_medany 1"), "{gcc15}");
            assert!(!gcc15.contains("cmodel_medlow") && !gcc15.contains("cmodel_pic"));
            let gcc12 = built(12, pic);
            assert!(has(&gcc12, "#define __riscv_cmodel_pic 1"), "{gcc12}");
            assert!(has(&gcc12, "#define __riscv_cmodel_medany 1"), "{gcc12}");
            let gcc10 = built(10, pic);
            assert!(has(&gcc10, "#define __riscv_cmodel_pic 1"), "{gcc10}");
            assert!(!gcc10.contains("cmodel_medany") && !gcc10.contains("cmodel_medlow"));
        }
        let absolute = built(12, Pic::Absolute);
        assert!(has(&absolute, "#define __riscv_cmodel_medlow 1"), "{absolute}");
        assert!(!absolute.contains("cmodel_pic") && !absolute.contains("cmodel_medany"));
    }

    /// tamnd/rucc#4088. gcc says `__HAVE_SPECULATION_SAFE_VALUE` where it has a speculation
    /// barrier, which is x86 and AArch64 here and not RISC-V. clang has no such builtin.
    #[test]
    fn the_speculation_macro_is_on_the_machines_with_a_barrier() {
        let said = |triple: &str| {
            let triple: Triple = triple.parse().expect("a triple");
            built_in(&TargetInfo::new(triple), &Predef::new())
        };
        let with = ["x86_64-unknown-freebsd", "i686-unknown-linux-gnu", "aarch64-unknown-netbsd"];
        for triple in with {
            assert!(has(&said(triple), "#define __HAVE_SPECULATION_SAFE_VALUE 1"), "{triple}");
        }
        let without = [
            "riscv64-unknown-freebsd",
            "aarch64-apple-darwin",
            "x86_64-pc-windows-msvc",
            "wasm32-unknown-unknown",
        ];
        for triple in without {
            assert!(!said(triple).contains("__HAVE_SPECULATION_SAFE_VALUE"), "{triple}");
        }
    }

    /// tamnd/rucc#4096. NetBSD's `_SOFT_FLOAT` says the machine has no float registers, which
    /// `-march=` decides and `-mabi=lp64` does not.
    #[test]
    fn netbsd_soft_float_follows_the_machine_and_not_the_abi() {
        let built = |march: Option<&str>| {
            let triple: Triple = "riscv64-unknown-netbsd".parse().expect("a triple");
            let opts = Predef {
                soft_float: true,
                vector: false,
                riscv_arch: march.map(str::to_owned),
                ..Predef::new()
            };
            built_in(&TargetInfo::new(triple), &opts)
        };
        assert!(!built(None).contains("_SOFT_FLOAT"));
        assert!(has(&built(Some("rv64imac")), "#define _SOFT_FLOAT 1"));
    }

    /// tamnd/rucc#4096. A kernel unit with no float unit is not IEC 60559 to gcc. x86 says
    /// `_SOFT_FLOAT` without the x87 as well, and gcc 12 has no `_Float16` there without SSE2.
    #[test]
    fn a_kernel_with_no_float_unit_is_not_iec_60559() {
        let gcc = |major: u32| GnucVersion { major, minor: 0, patch: 0 };
        let x86 = TargetInfo::new("x86_64-unknown-freebsd".parse().expect("a triple"));
        let kernel = |major: u32| Predef {
            gnuc: gcc(major),
            x87: false,
            vector: false,
            isa: Isa::NONE,
            hosted: false,
            ..Predef::new()
        };
        let text = built_in(&x86, &kernel(15));
        for line in ["_SOFT_FLOAT 1", "__GCC_IEC_559 0", "__GCC_IEC_559_COMPLEX 0"] {
            assert!(has(&text, &format!("#define {line}")), "{line}");
        }
        assert!(has(&text, "#define __FLT16_MANT_DIG__ 11"), "{text}");
        assert!(!built_in(&x86, &kernel(12)).contains("__FLT16_MANT_DIG__"));
        // The x87 alone, or SSE alone, is a float unit.
        let x87 = built_in(&x86, &Predef { x87: true, ..kernel(15) });
        assert!(has(&x87, "#define __GCC_IEC_559 2") && !x87.contains("_SOFT_FLOAT"));
        let sse = built_in(&x86, &Predef { x87: false, ..Predef::new() });
        assert!(has(&sse, "#define __GCC_IEC_559 2") && has(&sse, "#define _SOFT_FLOAT 1"));
        assert!(has(
            &built_in(&x86, &Predef { gnuc: gcc(12), ..Predef::new() }),
            "#define __FLT16_MANT_DIG__ 11"
        ));

        let arm = TargetInfo::new("aarch64-unknown-netbsd".parse().expect("a triple"));
        let text = built_in(&arm, &Predef { vector: false, ..Predef::new() });
        assert!(has(&text, "#define __GCC_IEC_559 0"), "{text}");
        assert!(!text.contains("_SOFT_FLOAT"), "{text}");
        let riscv = TargetInfo::new("riscv64-unknown-netbsd".parse().expect("a triple"));
        let rv = |march: &str| Predef { riscv_arch: Some(march.to_owned()), ..Predef::new() };
        assert!(has(&built_in(&riscv, &rv("rv64imac")), "#define __GCC_IEC_559 0"));
        assert!(has(&built_in(&riscv, &rv("rv64gc")), "#define __GCC_IEC_559 2"));
    }

    /// tamnd/rucc#4096. The guarded control stack, as gcc 15 says it.
    #[test]
    fn the_guarded_control_stack_is_said_the_way_the_acle_says_it() {
        let arm = TargetInfo::new("aarch64-unknown-freebsd".parse().expect("a triple"));
        assert!(!built_in(&arm, &Predef::new()).contains("__ARM_FEATURE_GCS_DEFAULT"));
        let branch_protection =
            BranchProtection { sign: SignReturn::NonLeaf, bti: true, gcs: true };
        let text = built_in(&arm, &Predef { branch_protection, ..Predef::new() });
        assert!(has(&text, "#define __ARM_FEATURE_GCS_DEFAULT 1"), "{text}");
    }

    /// tamnd/rucc#4088. gcc writes the AArch64 profile and float formats as numbers, and clang
    /// writes a character and hex.
    #[test]
    fn gcc_writes_the_aarch64_profile_and_float_formats_as_numbers() {
        for triple in ["aarch64-unknown-netbsd", "aarch64-unknown-freebsd", "aarch64-linux-gnu"] {
            let text = set_for(triple);
            assert!(has(&text, "#define __ARM_ARCH_PROFILE 65"), "{triple}");
            assert!(has(&text, "#define __ARM_FP 14"), "{triple}");
        }
        let apple = set_for("aarch64-apple-darwin");
        assert!(has(&apple, "#define __ARM_ARCH_PROFILE 'A'"), "{apple}");
        assert!(has(&apple, "#define __ARM_FP 0xe"), "{apple}");
    }

    #[test]
    fn a_version_field_is_the_digits_at_the_front_of_it() {
        assert_eq!(
            (field("0.10.68", 0), field("0.10.68", 1), field("0.10.68", 2)),
            ("0", "10", "68")
        );

        // A pre-release suffix belongs to the string and not to the number an `#if` compares, and
        // a field that is not there at all is what a two field version means by its third.
        assert_eq!(field("0.10.68-rc.1", 2), "68");
        assert_eq!(field("1.0", 2), "0");
        assert_eq!(field("", 0), "0");
    }

    #[test]
    fn assembly_is_told_it_is_assembly_and_loses_only_the_c_language_macros() {
        let triple: Triple = "x86_64-unknown-linux-gnu".parse().expect("a supported triple");
        let target = TargetInfo::new(triple);
        let c = built_in(&target, &Predef::new());
        let asm = built_in(&target, &Predef { assembler: true, ..Predef::new() });
        assert!(has(&asm, "#define __ASSEMBLER__ 1"), "{asm}");
        assert!(!c.contains("__ASSEMBLER__"), "{c}");

        // These are the lines gcc 16 leaves out of `-dM` for `assembler-with-cpp`, and nothing
        // else goes.
        let gone: Vec<&str> = c.lines().filter(|l| !asm.lines().any(|a| a == *l)).collect();
        let mut names: Vec<&str> =
            gone.iter().map(|l| l.split_whitespace().nth(1).unwrap_or_default()).collect();
        names.sort_unstable();
        assert_eq!(names, ["__STDC_UTF_16__", "__STDC_UTF_32__", "__STDC_VERSION__",]);
        assert!(has(&asm, "#define __x86_64__ 1") && has(&asm, "#define __STDC__ 1"), "{asm}");
    }

    /// tamnd/rucc#4088. Each BSD names itself as gcc's `TARGET_OS_CPP_BUILTINS` does, and none
    /// of them says what Linux or glibc's `<stdc-predef.h>` says.
    #[test]
    fn a_bsd_names_itself_as_its_gcc_does_and_says_nothing_of_linux() {
        let cases: [(&str, &[&str]); 7] = [
            (
                "x86_64-unknown-freebsd15.1",
                &["__FreeBSD__ 15", "__unix 1", "__unix__ 1", "unix 1", "__KPRINTF_ATTRIBUTE__ 1"],
            ),
            ("aarch64-unknown-freebsd", &["__FreeBSD__ 14", "__unix__ 1"]),
            ("x86_64-unknown-dragonfly", &["__DragonFly__ 1", "__unix 1", "unix 1"]),
            ("x86_64--netbsd", &["__NetBSD__ 1", "__unix__ 1", "__syslog_attribute__ 1"]),
            ("riscv64--netbsd", &["__NetBSD__ 1", "__riscv__ 1"]),
            ("i486--netbsdelf", &["__NetBSD__ 1", "__i386__ 1"]),
            ("x86_64-unknown-openbsd7.8", &["__OpenBSD__ 1", "__unix__ 1", "__ANSI_COMPAT 1"]),
        ];
        for (triple, lines) in cases {
            let set = set_for(triple);
            for line in lines.iter().chain(&["__ELF__ 1"]) {
                assert!(has(&set, &format!("#define {line}")), "{triple}: {line}");
            }
            for name in [
                "__linux__",
                "__linux",
                "linux",
                "__gnu_linux__",
                "__GLIBC_MINOR__",
                "__STDC_ISO_10646__",
                "__STDC_IEC_559__",
                "__STDC_IEC_559_COMPLEX__",
                "__STDC_IEC_60559_BFP__",
                "__STDC_IEC_60559_COMPLEX__",
            ] {
                assert!(!set.contains(&format!("#define {name} ")), "{triple}: {name}");
            }
        }
        // NetBSD and OpenBSD name `__unix__` alone, and `__riscv__` is NetBSD's.
        for triple in ["x86_64--netbsd", "x86_64-unknown-openbsd"] {
            let set = set_for(triple);
            assert!(!set.contains("#define __unix ") && !set.contains("#define unix "), "{triple}");
        }
        assert!(!set_for("riscv64-unknown-freebsd").contains("#define __riscv__ "));
        // `unix` is not a reserved name, so a strict mode leaves it to the program.
        let triple: Triple = "x86_64-unknown-freebsd".parse().unwrap();
        let strict =
            built_in(&TargetInfo::new(triple), &Predef { gnu_extensions: false, ..Predef::new() });
        assert!(has(&strict, "#define __unix__ 1") && !strict.contains("#define unix "));
    }

    /// tamnd/rucc#4088. The integer types are gcc's `freebsd-stdint.h`, `netbsd-stdint.h`,
    /// `dragonfly-stdint.h` and `openbsd-stdint.h`, with `wint_t` an `int` on all four.
    #[test]
    fn a_bsd_takes_the_integer_types_of_its_gcc() {
        let every = [
            "__WINT_TYPE__ int",
            "__WINT_MAX__ 0x7fffffff",
            "__WINT_MIN__ (-__WINT_MAX__ - 1)",
            "__INT_FAST8_TYPE__ int",
            "__UINT_FAST8_TYPE__ unsigned int",
            "__INT_FAST8_MAX__ 0x7fffffff",
            "__UINT_FAST8_MAX__ 0xffffffffU",
            "__INT_FAST8_WIDTH__ 32",
            "__INT_FAST16_TYPE__ int",
            "__INT_FAST16_WIDTH__ 32",
            "__INT_FAST32_TYPE__ int",
            "__SIG_ATOMIC_TYPE__ int",
        ];
        for triple in [
            "x86_64-unknown-freebsd",
            "i386-unknown-freebsd",
            "aarch64--netbsd",
            "x86_64-unknown-dragonfly",
            "x86_64-unknown-openbsd",
            "i386-unknown-openbsd",
        ] {
            let set = set_for(triple);
            for line in every {
                assert!(has(&set, &format!("#define {line}")), "{triple}: {line}");
            }
        }
        let freebsd = set_for("x86_64-unknown-freebsd");
        for line in ["__INT64_TYPE__ long int", "__INTMAX_TYPE__ long int", "__INTMAX_C(c) c ## L"]
        {
            assert!(has(&freebsd, &format!("#define {line}")), "{line}");
        }
        // OpenBSD makes every sixty four bit type a `long long`, and i386 OpenBSD makes the pointer
        // sized ones `long`.
        let openbsd = set_for("x86_64-unknown-openbsd");
        for line in [
            "__INT64_TYPE__ long long int",
            "__INT_LEAST64_TYPE__ long long int",
            "__INT_FAST64_TYPE__ long long int",
            "__INTMAX_TYPE__ long long int",
            "__UINTMAX_TYPE__ long long unsigned int",
            "__INTMAX_MAX__ 0x7fffffffffffffffLL",
            "__INTMAX_C(c) c ## LL",
            "__UINTMAX_C(c) c ## ULL",
            "__INTPTR_TYPE__ long int",
            "__SIZE_TYPE__ long unsigned int",
        ] {
            assert!(has(&openbsd, &format!("#define {line}")), "{line}");
        }
        let i386 = set_for("i386-unknown-openbsd");
        for line in [
            "__SIZE_TYPE__ long unsigned int",
            "__SIZE_MAX__ 0xffffffffUL",
            "__PTRDIFF_TYPE__ long int",
            "__INTPTR_TYPE__ long int",
            "__SIG_ATOMIC_MAX__ 0x7fffffff",
        ] {
            assert!(has(&i386, &format!("#define {line}")), "{line}");
        }
        assert!(has(&set_for("i386-unknown-freebsd"), "#define __SIZE_TYPE__ unsigned int"));
        // Linux keeps its own answers.
        let linux = set_for("x86_64-unknown-linux-gnu");
        assert!(has(&linux, "#define __WINT_TYPE__ unsigned int"));
        assert!(has(&linux, "#define __INT_FAST8_TYPE__ signed char"));
        assert!(has(&linux, "#define __INT_FAST8_WIDTH__ 8"));
    }

    /// tamnd/rucc#4088. i386 FreeBSD's `long double` is GCC's
    /// `ieee_extended_intel_96_round_53_format`, so its `float.h` values are a `double`'s
    /// precision in the x87 exponent range. `_Float64x` is the same mode and takes them too.
    #[test]
    fn i386_freebsd_describes_a_long_double_of_53_bits() {
        let freebsd = set_for("i386-unknown-freebsd14");
        for prefix in ["LDBL", "FLT64X"] {
            let suffix = if prefix == "LDBL" { "L" } else { "F64x" };
            for (name, value) in [
                ("MANT_DIG", "53".to_owned()),
                ("DIG", "15".to_owned()),
                ("MIN_EXP", "(-16381)".to_owned()),
                ("MIN_10_EXP", "(-4931)".to_owned()),
                ("MAX_EXP", "16384".to_owned()),
                ("MAX_10_EXP", "4932".to_owned()),
                ("DECIMAL_DIG", "17".to_owned()),
                ("MAX", format!("1.18973149535723163299902939989638351e+4932{suffix}")),
                ("NORM_MAX", format!("1.18973149535723163299902939989638351e+4932{suffix}")),
                ("MIN", format!("3.36210314311209350626267781732175260e-4932{suffix}")),
                ("EPSILON", format!("2.22044604925031308084726333618164062e-16{suffix}")),
                ("DENORM_MIN", format!("7.46536864129530798597817535205257178e-4948{suffix}")),
                ("IS_IEC_60559", "1".to_owned()),
            ] {
                let line = format!("#define __{prefix}_{name}__ {value}");
                assert!(has(&freebsd, &line), "{line}");
            }
        }
        assert!(has(&freebsd, "#define __DECIMAL_DIG__ 17"));
        assert!(has(&freebsd, "#define __SIZEOF_LONG_DOUBLE__ 12"));
        // The other i386 BSDs and x86-64 FreeBSD keep the sixty four bits of the x87 format.
        for triple in ["x86_64-unknown-freebsd", "i386-unknown-openbsd", "i486--netbsdelf"] {
            let set = set_for(triple);
            assert!(has(&set, "#define __LDBL_MANT_DIG__ 64"), "{triple}");
            assert!(has(&set, "#define __FLT64X_MANT_DIG__ 64"), "{triple}");
            assert!(has(&set, "#define __DECIMAL_DIG__ 21"), "{triple}");
        }
    }

    #[test]
    fn the_set_is_driven_by_the_target_rather_than_by_the_host() {
        let x86 = set_for("x86_64-unknown-linux-gnu");
        let arm = set_for("aarch64-unknown-linux-gnu");
        assert!(has(&x86, "#define __x86_64__ 1"));
        assert!(!has(&x86, "#define __aarch64__ 1"));
        assert!(has(&arm, "#define __aarch64__ 1"));
        assert!(!has(&arm, "#define __x86_64__ 1"));
        for side in [&x86, &arm] {
            assert!(has(side, "#define __GCC_ASM_FLAG_OUTPUTS__ 1"));
        }
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
    fn windows_spells_a_calling_convention_and_an_attribute_as_macros() {
        // The second declaration in mingw-w64's stdio.h is `int __cdecl __mingw_sscanf(...)`, so
        // a Windows target where these are missing reads no header at all.
        let windows = set_for("x86_64-pc-windows-gnu");
        assert!(has(&windows, "#define __cdecl __attribute__((__cdecl__))"));
        assert!(has(&windows, "#define __stdcall __attribute__((__stdcall__))"));
        assert!(has(&windows, "#define __fastcall __attribute__((__fastcall__))"));
        assert!(has(&windows, "#define __thiscall __attribute__((__thiscall__))"));
        assert!(has(&windows, "#define _cdecl __attribute__((__cdecl__))"));
        assert!(has(&windows, "#define __declspec(x) __attribute__((x))"));
        let linux = set_for("x86_64-unknown-linux-gnu");
        assert!(!has(&linux, "#define __cdecl __attribute__((__cdecl__))"));
        assert!(!has(&linux, "#define __declspec(x) __attribute__((x))"));
    }

    #[test]
    fn a_strict_mode_keeps_the_spellings_that_are_not_the_implementations_to_take() {
        // `_cdecl` is a name a program may use and `__cdecl` is not, so gcc defines the first
        // only where the extensions are on and the second everywhere.
        let mut opts = Predef::new();
        opts.gnu_extensions = false;
        let triple: Triple = "x86_64-pc-windows-gnu".parse().expect("a triple");
        let strict = built_in(&TargetInfo::new(triple), &opts);
        assert!(has(&strict, "#define __cdecl __attribute__((__cdecl__))"));
        assert!(!has(&strict, "#define _cdecl __attribute__((__cdecl__))"));
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
    fn apple_says_what_clang_for_apple_says_and_not_what_linux_does() {
        // Each line is the value clang 17 prints for `-target arm64-apple-macos11 -dM`.
        let darwin = set_for("aarch64-apple-darwin");
        for line in [
            "#define __INT64_TYPE__ long long int",
            "#define __UINT64_TYPE__ long long unsigned int",
            "#define __INT64_MAX__ 0x7fffffffffffffffLL",
            "#define __UINT64_MAX__ 0xffffffffffffffffULL",
            "#define __INT64_C(c) c ## LL",
            "#define __UINT64_C(c) c ## ULL",
            "#define __INT_LEAST64_TYPE__ long long int",
            "#define __INT_FAST64_TYPE__ long long int",
            "#define __UINT_FAST64_MAX__ 0xffffffffffffffffULL",
            // `intmax_t` and `size_t` stay `long` on Apple, which is why they are not the same
            // type as `int64_t` there.
            "#define __INTMAX_TYPE__ long int",
            "#define __SIZE_TYPE__ long unsigned int",
            "#define __BIGGEST_ALIGNMENT__ 8",
            "#define __GCC_DESTRUCTIVE_SIZE 128",
        ] {
            assert!(has(&darwin, line), "{line}");
        }
        for name in ["__unix__", "__unix", "unix", "__STDC_ISO_10646__"] {
            assert!(!darwin.contains(&format!("#define {name} ")), "{name}");
        }
        // The same processor under Linux keeps every one of them.
        let linux = set_for("aarch64-unknown-linux-gnu");
        assert!(has(&linux, "#define __INT64_TYPE__ long int"));
        assert!(has(&linux, "#define __BIGGEST_ALIGNMENT__ 16"));
        assert!(has(&linux, "#define __unix__ 1"));
        assert!(linux.contains("#define __STDC_ISO_10646__ "));
        // And an Intel Mac takes Apple's `int64_t` with its own alignment.
        let intel = set_for("x86_64-apple-darwin");
        assert!(has(&intel, "#define __INT64_TYPE__ long long int"));
        assert!(has(&intel, "#define __BIGGEST_ALIGNMENT__ 16"));
        assert!(!intel.contains("#define __unix__ "));
    }

    #[test]
    fn the_biggest_alignment_follows_avx_where_gcc_says_so() {
        // gcc 16.2.0 says 32 under `-mavx` and `-mfma` and 16 under `-mfma -mno-avx`. clang says
        // 16 under every flag, so Apple and MSVC keep 16.
        let biggest = |triple: &str, flags: &[&str]| {
            let triple: Triple = triple.parse().expect("a triple");
            let mut choices = rucc_target::Choices::new();
            for flag in flags {
                choices.read(flag).expect("a name gcc knows");
            }
            let opts = Predef { isa: choices.over(Isa::baseline()), ..Predef::new() };
            let text = built_in(&TargetInfo::new(triple), &opts);
            let line = text.lines().find(|line| line.starts_with("#define __BIGGEST_ALIGNMENT__ "));
            line.expect("defined").rsplit(' ').next().expect("a value").to_owned()
        };
        let linux = "x86_64-unknown-linux-gnu";
        assert_eq!(biggest(linux, &[]), "16");
        assert_eq!(biggest(linux, &["avx"]), "32");
        assert_eq!(biggest(linux, &["avx2"]), "32");
        assert_eq!(biggest(linux, &["fma"]), "32");
        assert_eq!(biggest(linux, &["fma", "no-avx"]), "16");
        assert_eq!(biggest("x86_64-pc-windows-gnu", &["avx"]), "32");
        assert_eq!(biggest("x86_64-pc-windows-msvc", &["avx"]), "16");
        assert_eq!(biggest("x86_64-apple-darwin", &["avx"]), "16");
        // gcc 16.2.0 says the same under `-m32`.
        assert_eq!(biggest("i686-unknown-linux-gnu", &[]), "16");
        assert_eq!(biggest("i686-unknown-linux-gnu", &["avx"]), "32");
        assert_eq!(biggest("i686-unknown-linux-gnu", &["fma", "no-avx"]), "16");
        assert_eq!(biggest("i686-pc-windows-msvc", &["avx"]), "16");
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
        // Mach-O keeps the underscore that ELF dropped, and so does i386 Windows.
        assert!(has(&set_for("aarch64-apple-darwin"), "#define __USER_LABEL_PREFIX__ _"));
        assert!(has(&set_for("i686-w64-windows-gnu"), "#define __USER_LABEL_PREFIX__ _"));
        assert!(has(&set_for("x86_64-w64-windows-gnu"), "#define __USER_LABEL_PREFIX__ "));
    }

    /// `-ffast-math` as gcc 16 spells it on x86-64 Linux: one macro per licence, the finite promise
    /// as a one, and the IEC 60559 family gone from both the compiler's lines and the ones glibc
    /// would write from `__GCC_IEC_559`.
    #[test]
    fn fast_math_defines_what_gcc_16_defines_for_it() {
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse().expect("a triple"));
        let mut opts = Predef::new();
        opts.trapping_math = opts.math.set_fast(true);
        let fast = built_in(&target, &opts);
        for line in [
            "#define __FAST_MATH__ 1",
            "#define __FINITE_MATH_ONLY__ 1",
            "#define __NO_MATH_ERRNO__ 1",
            "#define __NO_TRAPPING_MATH__ 1",
            "#define __NO_SIGNED_ZEROS__ 1",
            "#define __RECIPROCAL_MATH__ 1",
            "#define __ASSOCIATIVE_MATH__ 1",
            "#define __GCC_IEC_559 0",
            "#define __GCC_IEC_559_COMPLEX 0",
        ] {
            assert!(has(&fast, line), "{line}");
        }
        for name in ["__STDC_IEC_559__", "__STDC_IEC_559_COMPLEX__", "__STDC_IEC_60559_BFP__"] {
            assert!(!fast.contains(&format!("#define {name} ")), "{name}");
        }

        // `-ffast-math -ftrapping-math` keeps the members that do not need trapping off and
        // loses the name for the whole, and regrouping with it, which is gcc's answer.
        opts.trapping_math = true;
        let trapping = built_in(&target, &opts);
        assert!(has(&trapping, "#define __NO_MATH_ERRNO__ 1"));
        assert!(has(&trapping, "#define __GCC_IEC_559 0"));
        for name in ["__FAST_MATH__", "__NO_TRAPPING_MATH__", "__ASSOCIATIVE_MATH__"] {
            assert!(!trapping.contains(&format!("#define {name} ")), "{name}");
        }

        // And none of it by default.
        let plain = built_in(&target, &Predef::new());
        for name in ["__FAST_MATH__", "__NO_MATH_ERRNO__", "__NO_TRAPPING_MATH__"] {
            assert!(!plain.contains(&format!("#define {name} ")), "{name}");
        }
        assert!(has(&plain, "#define __STDC_IEC_559__ 1"));
    }

    /// `__EXCEPTIONS` is the one macro `-fexceptions` adds in C, and it is not there without it.
    #[test]
    fn exceptions_are_announced_to_the_headers_that_ask() {
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse().expect("a triple"));
        let mut opts = Predef::new();
        assert!(!built_in(&target, &opts).contains("__EXCEPTIONS"));
        opts.exceptions = true;
        assert!(has(&built_in(&target, &opts), "#define __EXCEPTIONS 1"));
    }

    /// The set gcc defines that headers read and that are true here. Written out one line at a
    /// time rather than counted, because the value is the whole point of each of them: a header
    /// asking `#if __FINITE_MATH_ONLY__` wants the number and not the existence.
    #[test]
    fn the_toolchain_macros_gcc_defines_are_defined_with_gccs_values() {
        let linux = set_for("x86_64-unknown-linux-gnu");
        for line in [
            "#define __GNUC_EXECUTION_CHARSET_NAME \"UTF-8\"",
            "#define __GNUC_WIDE_EXECUTION_CHARSET_NAME \"UTF-32LE\"",
            "#define __GXX_ABI_VERSION 1021",
            "#define __REGISTER_PREFIX__ ",
            "#define __PRAGMA_REDEFINE_EXTNAME 1",
            "#define __FINITE_MATH_ONLY__ 0",
            "#define __GCC_IEC_559 2",
            "#define __GCC_IEC_559_COMPLEX 2",
            "#define __GCC_CONSTRUCTIVE_SIZE 64",
            "#define __GCC_DESTRUCTIVE_SIZE 64",
            "#define __GCC_HAVE_SYNC_COMPARE_AND_SWAP_1 1",
            "#define __GCC_HAVE_SYNC_COMPARE_AND_SWAP_2 1",
            "#define __GCC_HAVE_SYNC_COMPARE_AND_SWAP_4 1",
            "#define __GCC_HAVE_SYNC_COMPARE_AND_SWAP_8 1",
            "#define __ATOMIC_HLE_ACQUIRE 65536",
            "#define __ATOMIC_HLE_RELEASE 131072",
            "#define __FXSR__ 1",
            "#define __MMX_WITH_SSE__ 1",
            "#define __code_model_small__ 1",
        ] {
            assert!(has(&linux, line), "{line}");
        }
        // The five that are the processor's rather than the compiler's stay on the processor.
        let arm = set_for("aarch64-unknown-linux-gnu");
        for name in ["__ATOMIC_HLE_ACQUIRE", "__FXSR__", "__MMX_WITH_SSE__", "__code_model_small__"]
        {
            assert!(!arm.contains(name), "{name}");
        }
        assert!(has(&arm, "#define __GCC_HAVE_SYNC_COMPARE_AND_SWAP_8 1"));
        // A wide character is sixteen bits on Windows, so a wide string is UTF-16 there.
        let windows = set_for("x86_64-pc-windows-msvc");
        assert!(has(&windows, "#define __GNUC_WIDE_EXECUTION_CHARSET_NAME \"UTF-16LE\""));
    }

    #[test]
    fn the_extensions_the_unit_is_built_for_are_macros() {
        // tamnd/rucc#2003. The baseline is four macros and SSE4.2 is six more, which is gcc 16's
        // `-msse4.2 -dM` output, and the unit's set is what decides it rather than the target.
        let triple: Triple = "x86_64-unknown-linux-gnu".parse().expect("a triple");
        let mut choices = rucc_target::Choices::new();
        choices.read("sse4.2").expect("a name gcc knows");
        let opts = Predef { isa: choices.over(Isa::baseline()), ..Predef::new() };
        let text = built_in(&TargetInfo::new(triple), &opts);
        for name in
            ["SSE", "SSE2", "MMX", "FXSR", "SSE3", "SSSE3", "SSE4_1", "SSE4_2", "POPCNT", "CRC32"]
        {
            assert!(has(&text, &format!("#define __{name}__ 1")), "{name}");
        }
        let plain = set_for("x86_64-unknown-linux-gnu");
        assert!(has(&plain, "#define __SSE2__ 1"));
        for name in ["__SSE3__", "__SSE4_2__", "__POPCNT__", "__CRC32__"] {
            assert!(!plain.contains(name), "{name}");
        }
        // AArch64 has none of them whatever the set says, since the set is an x86-64 one.
        let arm: Triple = "aarch64-unknown-linux-gnu".parse().expect("a triple");
        assert!(!built_in(&TargetInfo::new(arm), &opts).contains("__SSE"));
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
    fn the_size_of_int128_is_defined_only_where_a_register_is_sixty_four_bits() {
        let line = "#define __SIZEOF_INT128__ 16";
        assert!(has(&set_for("x86_64-unknown-linux-gnu"), line));
        assert!(has(&set_for("aarch64-unknown-linux-gnu"), line));
        assert!(!set_for_tuple("i686-linux-gnu").contains("__SIZEOF_INT128__"));
    }

    #[test]
    fn i686_says_it_is_an_i386_with_sse2_and_nothing_sixty_four_bit() {
        let i686 = set_for("i686-unknown-linux-gnu");
        for line in [
            "#define __i386__ 1",
            "#define __i386 1",
            "#define i386 1",
            "#define __i686__ 1",
            "#define __pentium4__ 1",
            "#define __SSE2__ 1",
            "#define __SSE2_MATH__ 1",
            "#define __FLT_EVAL_METHOD__ 0",
            "#define __ILP32__ 1",
            "#define _ILP32 1",
            "#define __code_model_32__ 1",
            "#define __GCC_ATOMIC_LLONG_LOCK_FREE 2",
            "#define __ATOMIC_HLE_ACQUIRE 65536",
            "#define __SIZEOF_POINTER__ 4",
            "#define __SIZE_TYPE__ unsigned int",
            "#define __PTRDIFF_TYPE__ int",
            "#define __INTPTR_TYPE__ int",
            "#define __UINTPTR_TYPE__ unsigned int",
            "#define __SIZE_MAX__ 0xffffffffU",
            "#define __PTRDIFF_MAX__ 0x7fffffff",
            "#define __INTMAX_TYPE__ long long int",
        ] {
            assert!(has(&i686, line), "{line}");
        }
        for name in ["__x86_64__", "__amd64__", "__LP64__", "__k8", "__code_model_small__"] {
            assert!(!i686.contains(name), "{name}");
        }
        let x86 = set_for("x86_64-unknown-linux-gnu");
        assert!(!x86.contains("__i386") && !x86.contains("ILP32"));
    }

    #[test]
    fn the_size_of_float128_is_defined_where_gcc_has_the_name() {
        let line = "#define __SIZEOF_FLOAT128__ 16";
        assert!(has(&set_for("x86_64-unknown-linux-gnu"), line));
        assert!(has(&set_for_tuple("i686-linux-gnu"), line));
        let power = set_for_tuple("powerpc64le-linux-gnu");
        assert!(has(&power, line));
        assert!(has(&power, "#define __FLOAT128__ 1"));
        assert!(!set_for("x86_64-unknown-linux-gnu").contains("__FLOAT128__"));
        assert!(!set_for("aarch64-unknown-linux-gnu").contains("__SIZEOF_FLOAT128__"));
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
    fn the_family_for_a_named_type_is_written_where_the_type_is_and_nowhere_else() {
        // gcc 13's rows, measured with the cross compilers: the half is on x86-64, AArch64 and
        // RISC-V, and the quad is on every one of the seven but armv7. A program asks the macro
        // to find out whether it may write the type, so a row where the two disagree is a header
        // that asks for a type the compiler then refuses.
        let has_family = |target: &str, prefix: &str| {
            set_for_tuple(target).contains(&format!("#define __{prefix}_MANT_DIG__ "))
        };
        assert!(has_family("x86_64-linux-gnu", "FLT16"));
        assert!(has_family("aarch64-linux-gnu", "FLT16"));
        assert!(has_family("riscv64-linux-gnu", "FLT16"));
        assert!(!has_family("i686-linux-gnu", "FLT16"));
        assert!(!has_family("s390x-linux-gnu", "FLT16"));
        assert!(!has_family("armv7-linux-gnueabihf", "FLT16"));

        assert!(has_family("i686-linux-gnu", "FLT128"));
        assert!(has_family("s390x-linux-gnu", "FLT128"));
        assert!(!has_family("armv7-linux-gnueabihf", "FLT128"));

        // The families every machine has are still there on the machine that has least, and so
        // is `_Float64x` on the machines that have a format for it.
        let arm = set_for_tuple("armv7-linux-gnueabihf");
        for prefix in ["FLT", "DBL", "LDBL", "FLT32", "FLT64", "FLT32X"] {
            assert!(arm.contains(&format!("#define __{prefix}_MANT_DIG__ ")), "__{prefix}_");
        }
        assert!(!arm.contains("__FLT64X_"));
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
    fn the_largest_value_of_an_ieee_format_is_also_its_largest_normal_one() {
        // `NORM_MAX` is only ever smaller than `MAX` for a format that holds values above its
        // largest normal one, and no IEEE encoding does. The double-double does, which is why
        // the two are separate fields now, and no target here has one.
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
    fn the_double_double_is_the_row_where_the_largest_value_is_not_the_largest_normal_one() {
        // No target here has a double-double `long double`, so this asks the row rather than the
        // macros. It is the reason `norm_max` is a field: a program that wants the largest value
        // it can still compute a full significand with wants `NORM_MAX`, and on PowerPC that is
        // a bit under half of `MAX`.
        let c = characteristics(Format::DoubleDouble);
        assert_ne!(c.max, c.norm_max);
        // `NORM_MAX` is two to the one thousand and twenty three, which is about half of `MAX`,
        // and `MAX` is a shade above `DBL_MAX` because the high half can be `DBL_MAX` and the low
        // half then adds to it. Both are what the reference prints.
        assert!(c.norm_max.starts_with("8.98846567431157953864652595394501e+307"));
        assert!(c.max.starts_with("1.7976931348623158"));
        // Epsilon is the odd one. The smallest value that changes a double-double near one is a
        // subnormal in the low half rather than one unit in the last place of anything, so it is
        // the same number as this format's own `DENORM_MIN`, which no other row can say.
        assert_eq!(c.epsilon, c.denorm_min);
        for format in [Format::Half, Format::Single, Format::Double, Format::X87Extended] {
            assert_ne!(characteristics(format).epsilon, characteristics(format).denorm_min);
        }
        // And it is not an IEC 60559 format, which only the brain float can also say.
        assert_eq!(c.is_iec_60559, "0");
    }

    /// The set under a persona that claims gcc `major`, in the dialect that release compiles
    /// when it is not given one.
    fn set_claiming(triple: &str, major: u32) -> String {
        let triple: Triple = triple.parse().expect("a triple the compiler supports");
        let gnuc = GnucVersion { major, minor: 0, patch: 0 };
        let opts = Predef { gnuc, gnuc_given: true, std: gnuc.default_std(), ..Predef::new() };
        built_in(&TargetInfo::new(triple), &opts)
    }

    /// tamnd/rucc#4088. The names that came in a gcc release are there when the persona claims
    /// that release or a later one, with the value that release gives. The values are what the
    /// gcc 10, 12 and 14 of NetBSD 10, 11 and current, and FreeBSD's gcc 14 and 15, give.
    #[test]
    fn the_names_that_came_in_a_gcc_release_follow_the_claimed_one() {
        let x86 = "x86_64-unknown-netbsd";
        let gcc10 = set_claiming(x86, 10);
        assert!(has(&gcc10, "#define __GXX_ABI_VERSION 1014"), "{gcc10}");
        assert!(has(&gcc10, "#define __DEC_EVAL_METHOD__ 2"), "{gcc10}");
        for name in [
            "__GNUC_EXECUTION_CHARSET_NAME ",
            "__GNUC_WIDE_EXECUTION_CHARSET_NAME ",
            "__FLT_IS_IEC_60559__ ",
            "__FLT16_MANT_DIG__ ",
            "__GCC_CONSTRUCTIVE_SIZE ",
            "__BITINT_MAXWIDTH__ ",
            "__STDC_EMBED_FOUND__ ",
            "__GCC_ATOMIC_CHAR8_T_LOCK_FREE ",
            "__FLT_EVAL_METHOD_C99__ ",
        ] {
            assert!(!gcc10.contains(&format!("#define {name}")), "{name}");
        }
        let gcc12 = set_claiming(x86, 12);
        for line in [
            "__GXX_ABI_VERSION 1017",
            "__GNUC_EXECUTION_CHARSET_NAME \"UTF-8\"",
            "__FLT_IS_IEC_60559__ 2",
            "__LDBL_IS_IEC_60559__ 2",
            "__FLT16_MANT_DIG__ 11",
            "__GCC_CONSTRUCTIVE_SIZE 64",
            "__GCC_DESTRUCTIVE_SIZE 64",
        ] {
            assert!(has(&gcc12, &format!("#define {line}")), "{line}");
        }
        assert!(!gcc12.contains("__BITINT_MAXWIDTH__"));
        let gcc14 = set_claiming("x86_64-unknown-freebsd", 14);
        for line in ["__GXX_ABI_VERSION 1019", "__FLT_IS_IEC_60559__ 1", "__BITINT_MAXWIDTH__ 128"]
        {
            assert!(has(&gcc14, &format!("#define {line}")), "{line}");
        }
        assert!(!gcc14.contains("__STDC_EMBED_"));
        let gcc15 = set_claiming("x86_64-unknown-freebsd", 15);
        for line in [
            "__GXX_ABI_VERSION 1020",
            "__STDC_EMBED_FOUND__ 1",
            "__GCC_ATOMIC_CHAR8_T_LOCK_FREE 2",
            "__GCC_HAVE_DWARF2_CFI_ASM 1",
        ] {
            assert!(has(&gcc15, &format!("#define {line}")), "{line}");
        }
        assert!(has(&set_for("x86_64-unknown-linux-gnu"), "#define __GXX_ABI_VERSION 1021"));
    }

    /// tamnd/rucc#4088. The brain float's family is there where `__bf16` is a type, which is from
    /// gcc 13 on x86-64 and AArch64. The values are the reference rows'.
    #[test]
    fn the_brain_float_family_is_where_gcc_has_the_type() {
        for (triple, since) in [
            ("x86_64-unknown-freebsd", 13),
            ("aarch64-unknown-freebsd", 13),
            ("aarch64-unknown-netbsd", 13),
        ] {
            assert!(!set_claiming(triple, since - 1).contains("__BFLT16_"), "{triple}");
            let text = set_claiming(triple, since);
            for line in [
                "__BFLT16_MANT_DIG__ 8",
                "__BFLT16_DIG__ 2",
                "__BFLT16_MIN_EXP__ (-125)",
                "__BFLT16_MAX_10_EXP__ 38",
                "__BFLT16_DECIMAL_DIG__ 4",
                "__BFLT16_MAX__ 3.38953138925153547590470800371487867e+38BF16",
                "__BFLT16_NORM_MAX__ 3.38953138925153547590470800371487867e+38BF16",
                "__BFLT16_MIN__ 1.17549435082228750796873653722224568e-38BF16",
                "__BFLT16_EPSILON__ 7.81250000000000000000000000000000000e-3BF16",
                "__BFLT16_DENORM_MIN__ 9.18354961579912115600575419704879436e-41BF16",
                "__BFLT16_IS_IEC_60559__ 0",
                "__BFLT16_HAS_QUIET_NAN__ 1",
            ] {
                assert!(has(&text, &format!("#define {line}")), "{triple}: {line}");
            }
        }
        // Not on i386 and RISC-V (tamnd/rucc#4276), where rucc has no `__bf16` yet, and not where
        // gcc has no type.
        for triple in
            ["i686-unknown-linux-gnu", "riscv64-unknown-freebsd", "wasm32-unknown-unknown"]
        {
            assert!(!set_claiming(triple, 15).contains("__BFLT16_"), "{triple}");
        }
    }

    /// tamnd/rucc#4088. Some of those names depend on the machine too. AArch64 had `_Float16`
    /// before gcc 10 and is the one machine with `__FLT_EVAL_METHOD_C99__`. RISC-V got
    /// `_Float16` in gcc 13 and has no cache line size and no `_BitInt` in gcc 15, and FreeBSD's
    /// gcc for it writes no CFI.
    #[test]
    fn the_names_of_a_gcc_release_follow_the_machine_as_well() {
        let arm = set_claiming("aarch64-unknown-netbsd", 10);
        assert!(has(&arm, "#define __FLT16_MANT_DIG__ 11"), "{arm}");
        assert!(has(&arm, "#define __FLT_EVAL_METHOD_C99__ 0"), "{arm}");
        assert!(!arm.contains("__GCC_DESTRUCTIVE_SIZE"), "{arm}");
        assert!(has(
            &set_claiming("aarch64-unknown-netbsd", 12),
            "#define __GCC_DESTRUCTIVE_SIZE 256"
        ));
        assert!(!set_claiming("riscv64-unknown-netbsd", 12).contains("__FLT16_MANT_DIG__"));
        let riscv = set_claiming("riscv64-unknown-netbsd", 13);
        assert!(has(&riscv, "#define __FLT16_MANT_DIG__ 11"), "{riscv}");
        assert!(has(&riscv, "#define __GCC_HAVE_DWARF2_CFI_ASM 1"), "{riscv}");
        let freebsd = set_claiming("riscv64-unknown-freebsd", 15);
        for name in [
            "__GCC_CONSTRUCTIVE_SIZE",
            "__BITINT_MAXWIDTH__",
            "__GCC_HAVE_DWARF2_CFI_ASM",
            "__FLT_EVAL_METHOD_C99__",
        ] {
            assert!(!freebsd.contains(name), "{name}");
        }
        assert!(!set_for("x86_64-unknown-linux-gnu").contains("__FLT_EVAL_METHOD_C99__"));
        // No table, no directives.
        let triple: Triple = "x86_64-unknown-linux-gnu".parse().expect("a triple");
        let quiet = built_in(&TargetInfo::new(triple), &Predef { unwinds: false, ..Predef::new() });
        assert!(!quiet.contains("__GCC_HAVE_DWARF2_CFI_ASM"));
    }

    /// `u"..."` and `U"..."` are literals in gnu99 and from C11 on, and gcc says the two names
    /// there and nowhere else. `char8_t` is a C23 type, and gcc 15 says its lock free answer in
    /// C23 alone.
    #[test]
    fn the_unicode_names_follow_the_dialect() {
        let target = TargetInfo::new("x86_64-unknown-freebsd".parse().expect("a triple"));
        let gcc15 = GnucVersion { major: 15, minor: 0, patch: 0 };
        for (std, gnu, utf, char8) in [
            (Std::C89, true, false, false),
            (Std::C99, false, false, false),
            (Std::C99, true, true, false),
            (Std::C11, false, true, false),
            (Std::C17, true, true, false),
            (Std::C23, true, true, true),
        ] {
            let opts = Predef { std, gnu_extensions: gnu, gnuc: gcc15, ..Predef::new() };
            let text = built_in(&target, &opts);
            assert_eq!(has(&text, "#define __STDC_UTF_16__ 1"), utf, "{std:?} {gnu}");
            assert_eq!(has(&text, "#define __STDC_UTF_32__ 1"), utf, "{std:?} {gnu}");
            let lock_free = has(&text, "#define __GCC_ATOMIC_CHAR8_T_LOCK_FREE 2");
            assert_eq!(lock_free, char8, "{std:?} {gnu}");
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
    /// not true changes what a header declares rather than turning anything off. gcc 16 makes
    /// none of them, and every one of the four features is here.
    #[test]
    fn nothing_is_claimed_missing_since_nothing_is() {
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse().unwrap());
        for std in [Std::C11, Std::C17, Std::C23] {
            let set = built_in(&target, &Predef { std, ..Predef::new() });
            assert!(!set.contains("__STDC_NO_"), "{std:?}");
        }
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

    /// glibc's `<stdc-predef.h>` is read before the first line of every translation unit and
    /// writes `#define __STDC_IEC_60559_BFP__ 201404L` whenever `__GCC_IEC_559` is positive.
    /// Any other value here is a redefinition with a different body, which is a warning the
    /// program did not ask for and which a configure script reads as a failed feature test.
    #[test]
    fn the_ieee_annex_macro_carries_the_value_glibcs_own_header_writes() {
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse().unwrap());
        let mut opts = Predef::new();
        for std in [Std::C23, Std::C17, Std::C11, Std::C99, Std::C89] {
            opts.std = std;
            let set = built_in(&target, &opts);
            assert!(has(&set, "#define __STDC_IEC_60559_BFP__ 201404L"), "{std:?}");
            assert!(has(&set, "#define __STDC_IEC_60559_COMPLEX__ 201404L"), "{std:?}");
            // The two the library reads to decide whether to write the four above itself.
            assert!(has(&set, "#define __GCC_IEC_559 2"), "{std:?}");
            assert!(has(&set, "#define __GCC_IEC_559_COMPLEX 2"), "{std:?}");
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
        assert!(has(&built_in(&target, &opts), "#define __GNUC__ 16"));
        opts.gnuc = GnucVersion { major: 15, minor: 1, patch: 0 };
        assert!(has(&built_in(&target, &opts), "#define __GNUC__ 15"));
        assert!(has(&built_in(&target, &opts), "#define __GNUC_MINOR__ 1"));
    }

    /// Which of the two inline macros is defined, over the two things that decide it.
    ///
    /// Exactly one of them is defined at a time, which is what a header reads: glibc's
    /// `__extern_inline` writes `extern __inline` under one and adds `__gnu_inline__` under the
    /// other, so both being defined or neither being defined is a header taking a path it was
    /// never meant to take.
    #[test]
    fn one_of_the_two_inline_macros_is_defined_and_three_things_can_pick_which() {
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse().unwrap());
        let gnu = "#define __GNUC_GNU_INLINE__ 1";
        let stdc = "#define __GNUC_STDC_INLINE__ 1";

        let mut opts = Predef::new();
        assert!(has(&built_in(&target, &opts), stdc));
        assert!(!has(&built_in(&target, &opts), gnu));

        opts.gnu89_inline = true;
        assert!(has(&built_in(&target, &opts), gnu));
        assert!(!has(&built_in(&target, &opts), stdc));

        // The dialect on its own, which is where the older reading came from.
        let mut opts = Predef::new();
        opts.std = Std::C89;
        assert!(has(&built_in(&target, &opts), gnu));
        assert!(!has(&built_in(&target, &opts), stdc));
    }

    #[test]
    fn a_darwin_target_says_its_deployment_target_as_the_sdk_reads_it() {
        let default = set_for("aarch64-apple-darwin");
        assert!(has(&default, "#define __ENVIRONMENT_MAC_OS_X_VERSION_MIN_REQUIRED__ 110000"));
        assert!(has(&default, "#define __ENVIRONMENT_OS_VERSION_MIN_REQUIRED__ 110000"));
        let pinned = set_for_tuple("aarch64-macos.13.4");
        assert!(has(&pinned, "#define __ENVIRONMENT_MAC_OS_X_VERSION_MIN_REQUIRED__ 130400"));
        assert!(has(&pinned, "#define __ENVIRONMENT_OS_VERSION_MIN_REQUIRED__ 130400"));
        let linux = set_for("aarch64-unknown-linux-gnu");
        assert!(!linux.contains("VERSION_MIN_REQUIRED"));
    }

    #[test]
    fn apple_arm64_has_the_older_spellings_its_sdk_reads() {
        let darwin = set_for("aarch64-apple-darwin");
        for line in [
            "#define __ARM64_ARCH_8__ 1",
            "#define __ARM_NEON__ 1",
            "#define __AARCH64_SIMD__ 1",
            "#define __LITTLE_ENDIAN__ 1",
        ] {
            assert!(has(&darwin, line), "{line}");
        }
        // gcc defines none of them for Linux, and a header that tests `__LITTLE_ENDIAN__` there
        // is one written for clang that gcc users already build without it.
        let linux = set_for("aarch64-unknown-linux-gnu");
        assert!(!linux.contains("__ARM_NEON__"));
        assert!(!linux.contains("#define __LITTLE_ENDIAN__"));
    }

    #[test]
    fn sixty_four_bit_glibc_makes_the_fast_types_long_on_every_processor() {
        // glibc's `stdint.h` picks `long` under `__WORDSIZE == 64` and never asks which
        // processor it is on, and gcc for aarch64, riscv64 and powerpc64le all agree.
        for triple in ["aarch64-unknown-linux-gnu", "riscv64-unknown-linux-gnu"] {
            let set = set_for(triple);
            assert!(has(&set, "#define __INT_FAST16_TYPE__ long int"), "{triple}");
            assert!(has(&set, "#define __UINT_FAST32_TYPE__ long unsigned int"), "{triple}");
            assert!(has(&set, "#define __INT_FAST32_WIDTH__ 64"), "{triple}");
        }
        assert!(has(&set_for("aarch64-unknown-linux-musl"), "#define __INT_FAST16_TYPE__ int"));
        // Apple's `stdint.h` makes the 16 bit one `int16_t` and the 32 bit one `int32_t`.
        let darwin = set_for("aarch64-apple-darwin");
        assert!(has(&darwin, "#define __INT_FAST16_TYPE__ short int"));
        assert!(has(&darwin, "#define __INT_FAST32_TYPE__ int"));
    }

    #[test]
    fn armv8_a_says_what_the_base_architecture_has() {
        let arm = set_for("aarch64-unknown-linux-gnu");
        for line in [
            "#define __ARM_ARCH_ISA_A64 1",
            "#define __ARM_FEATURE_CLZ 1",
            "#define __ARM_FEATURE_IDIV 1",
            "#define __ARM_FEATURE_FMA 1",
            "#define __FP_FAST_FMA 1",
            "#define __GCC_DESTRUCTIVE_SIZE 256",
        ] {
            assert!(has(&arm, line), "{line}");
        }
        let x86 = set_for("x86_64-unknown-linux-gnu");
        assert!(!x86.contains("__ARM_FEATURE_CLZ"));
        assert!(!x86.contains("__FP_FAST_FMA"));
        assert!(has(&x86, "#define __GCC_DESTRUCTIVE_SIZE 64"));
    }

    #[test]
    fn the_crc32_macro_follows_the_aarch64_march() {
        // tamnd/rucc#2006, and gcc 16's `-dM` for aarch64-linux-gnu under each `-march=`.
        let arm: Triple = "aarch64-unknown-linux-gnu".parse().expect("a triple");
        let macro_line = "#define __ARM_FEATURE_CRC32 1";
        for (march, defined) in [
            ("armv8-a", false),
            ("armv8-a+crc", true),
            ("armv8-a+crc+simd", true),
            ("armv8.1-a", true),
            ("armv9-a", true),
            ("armv8.1-a+nocrc", false),
        ] {
            let opts = Predef { isa: Isa::aarch64_march(march), ..Predef::new() };
            let text = built_in(&TargetInfo::new(arm), &opts);
            assert_eq!(has(&text, macro_line), defined, "{march}");
        }
        assert!(!set_for("aarch64-unknown-linux-gnu").contains("__ARM_FEATURE_CRC32"));
        // An x86-64 unit has no such thing whatever it is built for.
        let x86: Triple = "x86_64-unknown-linux-gnu".parse().expect("a triple");
        let opts = Predef { isa: Isa::aarch64_march("armv8-a+crc"), ..Predef::new() };
        assert!(!built_in(&TargetInfo::new(x86), &opts).contains("__ARM_FEATURE_CRC32"));
    }

    /// tamnd/rucc#4088. gcc says `__ARM_FEATURE_ATOMICS` for the large system extensions, and
    /// gcc and clang both say `__GCC_HAVE_SYNC_COMPARE_AND_SWAP_16` on every AArch64 target.
    #[test]
    fn the_aarch64_atomics_macros_follow_the_march_and_the_machine() {
        let arm: Triple = "aarch64-unknown-freebsd".parse().expect("a triple");
        for (march, defined) in [
            ("armv8-a", false),
            ("armv8-a+lse", true),
            ("armv8.1-a", true),
            ("armv9-a", true),
            ("armv8.1-a+nolse", false),
        ] {
            let opts = Predef { isa: Isa::aarch64_march(march), ..Predef::new() };
            let text = built_in(&TargetInfo::new(arm), &opts);
            assert_eq!(has(&text, "#define __ARM_FEATURE_ATOMICS 1"), defined, "{march}");
        }
        let sixteen = "#define __GCC_HAVE_SYNC_COMPARE_AND_SWAP_16 1";
        let with = ["aarch64-unknown-freebsd", "aarch64-unknown-netbsd", "aarch64-apple-darwin"];
        for triple in with {
            assert!(has(&set_for(triple), sixteen), "{triple}");
        }
        let without = ["x86_64-unknown-freebsd", "riscv64-unknown-freebsd", "i686-linux-gnu"];
        for triple in without {
            assert!(!set_for(triple).contains("COMPARE_AND_SWAP_16"), "{triple}");
        }
    }

    #[test]
    fn a_unit_built_for_fma_says_the_fused_multiply_add_is_fast_where_gcc_says_so() {
        // gcc 16.2.0 with `-mfma` defines the five on x86-64, and under `-m32` with
        // `-mfpmath=sse`, which is what i386 always is here. clang 18 defines none on any x86
        // row.
        let built = |triple: &str, flag: &str| {
            let triple: Triple = triple.parse().expect("a triple");
            let mut choices = rucc_target::Choices::new();
            choices.read(flag).expect("a name gcc knows");
            let opts = Predef { isa: choices.over(Isa::baseline()), ..Predef::new() };
            built_in(&TargetInfo::new(triple), &opts)
        };
        for triple in
            ["x86_64-unknown-linux-gnu", "x86_64-w64-windows-gnu", "i686-unknown-linux-gnu"]
        {
            let text = built(triple, "fma");
            for name in ["", "F", "F32", "F64", "F32x"] {
                assert!(has(&text, &format!("#define __FP_FAST_FMA{name} 1")), "{triple} {name}");
            }
            assert!(!built(triple, "avx2").contains("__FP_FAST_FMA"), "{triple}");
        }
        for triple in ["x86_64-apple-darwin", "x86_64-pc-windows-msvc", "i686-pc-windows-msvc"] {
            assert!(!built(triple, "fma").contains("__FP_FAST_FMA"), "{triple}");
        }
    }

    #[test]
    fn an_i386_unit_with_floats_on_the_x87_stack_makes_no_promise_about_sse_math() {
        // gcc 13 with `-m32 -mfpmath=387 -mfma` keeps `__SSE2__` and `__FMA__` and drops the two
        // math macros and the five fused multiply add ones.
        let triple: Triple = "i686-unknown-linux-gnu".parse().expect("a triple");
        let mut choices = rucc_target::Choices::new();
        choices.read("fma").expect("a name gcc knows");
        let isa = choices.over(Isa::baseline());
        let sse = built_in(&TargetInfo::new(triple), &Predef { isa, ..Predef::new() });
        let x87 = Predef { isa, x87_floats: true, ..Predef::new() };
        let x87 = built_in(&TargetInfo::new(triple), &x87);
        for name in ["__SSE_MATH__ ", "__SSE2_MATH__ ", "__FP_FAST_FMA "] {
            assert!(sse.contains(name), "{name}");
            assert!(!x87.contains(name), "{name}");
        }
        for name in ["__SSE2__ ", "__FMA__ "] {
            assert!(x87.contains(name), "{name}");
        }
    }

    #[test]
    fn an_i386_unit_with_floats_on_the_x87_stack_evaluates_them_in_long_double() {
        // gcc 13 for i686 says 2 for both names with the x87 math it has by default, and 0 with
        // `-msse2 -mfpmath=sse`, in every dialect.
        let triple: Triple = "i686-unknown-linux-gnu".parse().expect("a triple");
        let sse = built_in(&TargetInfo::new(triple), &Predef::new());
        let x87 = Predef { x87_floats: true, ..Predef::new() };
        let x87 = built_in(&TargetInfo::new(triple), &x87);
        for name in ["__FLT_EVAL_METHOD__", "__FLT_EVAL_METHOD_TS_18661_3__"] {
            assert!(sse.contains(&format!("#define {name} 0\n")), "{name}");
            assert!(x87.contains(&format!("#define {name} 2\n")), "{name}");
        }
    }

    #[test]
    fn an_i386_unit_defines_the_macros_of_the_extensions_the_flags_add() {
        // gcc 16.2.0 with `-m32 -march=pentium4 -mfpmath=sse` and each flag. The baseline's four
        // are defined whatever the flags say, as they were before.
        let built = |flags: &[&str]| {
            let triple: Triple = "i686-unknown-linux-gnu".parse().expect("a triple");
            let mut choices = rucc_target::Choices::new();
            for flag in flags {
                choices.read(flag).expect("a name gcc knows");
            }
            let opts = Predef { isa: choices.over(Isa::baseline()), ..Predef::new() };
            built_in(&TargetInfo::new(triple), &opts)
        };
        let fma = built(&["fma"]);
        for name in ["AVX", "CRC32", "FMA", "POPCNT", "SSE3", "SSSE3", "SSE4_1", "SSE4_2", "XSAVE"]
        {
            assert!(has(&fma, &format!("#define __{name}__ 1")), "{name}");
        }
        assert!(!fma.contains("__AVX2__") && !fma.contains("__F16C__"));
        let popcnt = built(&["popcnt"]);
        assert!(has(&popcnt, "#define __POPCNT__ 1") && !popcnt.contains("__SSE3__"));
        assert!(has(&built(&["sahf"]), "#define __LAHF_SAHF__ 1"));
        let kernel = built(&["no-sse", "no-mmx", "no-sse2"]);
        for name in ["MMX", "SSE", "SSE2", "FXSR"] {
            assert!(has(&kernel, &format!("#define __{name}__ 1")), "{name}");
        }
        assert!(!kernel.contains("__SSE3__"));
    }

    #[test]
    fn a_unit_without_vector_registers_says_so_the_way_gcc_does() {
        // tamnd/rucc#2277. `-mno-sse` takes the floating point macros away along with the
        // extensions, and `-mgeneral-regs-only` on AArch64 takes FP and NEON away.
        let x86: Triple = "x86_64-unknown-linux-gnu".parse().expect("a triple");
        let mut choices = rucc_target::Choices::new();
        choices.read("no-sse").expect("a name gcc knows");
        let opts = Predef { isa: choices.over(Isa::baseline()), vector: false, ..Predef::new() };
        let text = built_in(&TargetInfo::new(x86), &opts);
        for name in ["__SSE__", "__SSE2__", "__SSE_MATH__", "__SSE2_MATH__", "__MMX_WITH_SSE__"] {
            assert!(!text.contains(name), "{name}");
        }
        assert!(has(&set_for("x86_64-unknown-linux-gnu"), "#define __SSE2_MATH__ 1"));
        let arm: Triple = "aarch64-unknown-linux-gnu".parse().expect("a triple");
        let opts = Predef { vector: false, ..Predef::new() };
        let text = built_in(&TargetInfo::new(arm), &opts);
        for name in ["__ARM_FP ", "__ARM_NEON ", "__ARM_FEATURE_FMA", "__FP_FAST_FMA"] {
            assert!(!text.contains(name), "{name}");
        }
        assert!(has(&text, "#define __ARM_FEATURE_IDIV 1"));
        assert!(has(&set_for("aarch64-unknown-linux-gnu"), "#define __ARM_NEON 1"));
    }

    /// tamnd/rucc#4088. gcc on AArch64 says `__fp16` is the IEEE half and may be passed, and
    /// takes both away with the floating registers. clang 23 says both even with
    /// `-mgeneral-regs-only`. No other machine has the type.
    #[test]
    fn the_fp16_macros_are_on_aarch64_where_the_type_is() {
        let names = ["#define __ARM_FP16_FORMAT_IEEE 1", "#define __ARM_FP16_ARGS 1"];
        for triple in ["aarch64-unknown-freebsd15.1", "aarch64-unknown-netbsd10.2"] {
            let text = set_for(triple);
            for name in names {
                assert!(has(&text, name), "{triple}: {name}");
            }
            let target = TargetInfo::new(triple.parse().expect("a triple"));
            let bare = built_in(&target, &Predef { vector: false, ..Predef::new() });
            assert!(!bare.contains("__ARM_FP16_"), "{triple}");
        }
        let apple = TargetInfo::new("aarch64-apple-darwin".parse().expect("a triple"));
        let bare = built_in(&apple, &Predef { vector: false, ..Predef::new() });
        for name in names {
            assert!(has(&bare, name), "{name}");
        }
        for triple in ["x86_64-unknown-freebsd15.1", "riscv64-unknown-freebsd15.1"] {
            assert!(!set_for(triple).contains("__ARM_FP16_"), "{triple}");
        }
    }

    /// tamnd/rucc#4225. gcc 14 defines the SME keyword attributes on every AArch64 target, and
    /// gcc 13 does not. clang defines none of them as macros.
    #[test]
    fn the_sme_keyword_attributes_are_macros_from_gcc_14_on() {
        let gcc = |major: u32| Predef {
            gnuc: GnucVersion { major, minor: 0, patch: 0 },
            ..Predef::new()
        };
        for triple in ["aarch64-unknown-freebsd15.1", "aarch64-unknown-netbsd10.2"] {
            let target = TargetInfo::new(triple.parse().expect("a triple"));
            let text = built_in(&target, &gcc(14));
            for line in [
                "#define __ARM_STATE_ZA 1",
                "#define __ARM_STATE_ZT0 1",
                "#define __arm_streaming [[__extension__ arm::streaming]]",
                "#define __arm_locally_streaming [[__extension__ arm::locally_streaming]]",
                "#define __arm_inout(...) [[__extension__ arm::inout(__VA_ARGS__)]]",
                "#define __arm_new(...) [[__extension__ arm::new(__VA_ARGS__)]]",
            ] {
                assert!(has(&text, line), "{triple}: {line}");
            }
            assert!(!text.contains("__ARM_NEON_SVE_BRIDGE"), "{triple}");
            let old = built_in(&target, &gcc(13));
            assert!(!old.contains("__ARM_STATE_") && !old.contains("__arm_"), "{triple}");
        }
        let apple = TargetInfo::new("aarch64-apple-darwin".parse().expect("a triple"));
        assert!(!built_in(&apple, &gcc(14)).contains("__arm_streaming"));
        let x86 = TargetInfo::new("x86_64-unknown-freebsd15.1".parse().expect("a triple"));
        assert!(!built_in(&x86, &gcc(14)).contains("__ARM_STATE_ZA"));
    }

    #[test]
    fn branch_protection_is_said_the_way_the_acle_says_it() {
        let arm = TargetInfo::new("aarch64-unknown-linux-gnu".parse().expect("a triple"));
        let plain = built_in(&arm, &Predef::new());
        assert!(!plain.contains("__ARM_FEATURE_BTI_DEFAULT"));
        assert!(!plain.contains("__ARM_FEATURE_PAC_DEFAULT"));
        for (sign, pac) in [(SignReturn::NonLeaf, "1"), (SignReturn::All, "5")] {
            let branch_protection = BranchProtection { sign, bti: true, gcs: false };
            let text = built_in(&arm, &Predef { branch_protection, ..Predef::new() });
            assert!(has(&text, "#define __ARM_FEATURE_BTI_DEFAULT 1"), "{text}");
            assert!(has(&text, &format!("#define __ARM_FEATURE_PAC_DEFAULT {pac}")), "{text}");
        }
    }

    /// tamnd/rucc#4095. NetBSD builds libgcc with `-fbuilding-libgcc`. The lines are what
    /// `x86_64-linux-gnu-gcc` 15.1 prints for `-fbuilding-libgcc -dM -E`, less the `bfloat16`
    /// mode's limits, which name a `float.h` family this compiler does not define.
    #[test]
    fn building_libgcc_defines_the_macros_gcc_gives_libgcc() {
        let libgcc = |triple: &str| {
            let target = TargetInfo::new(triple.parse().expect("a triple"));
            let text = built_in(&target, &Predef { building_libgcc: true, ..Predef::new() });
            let mut lines: Vec<String> =
                text.lines().filter(|line| line.contains("__LIBGCC_")).map(str::to_owned).collect();
            lines.sort();
            lines
        };
        let gcc = [
            "#define __LIBGCC_BF_MANT_DIG__ 8",
            "#define __LIBGCC_BITINT_LIMB_WIDTH__ 64",
            "#define __LIBGCC_BITINT_ORDER__ __ORDER_LITTLE_ENDIAN__",
            "#define __LIBGCC_DF_EPSILON__ __DBL_EPSILON__",
            "#define __LIBGCC_DF_EXCESS_PRECISION__ 0",
            "#define __LIBGCC_DF_FUNC_EXT__ ",
            "#define __LIBGCC_DF_MANT_DIG__ 53",
            "#define __LIBGCC_DF_MAX__ __DBL_MAX__",
            "#define __LIBGCC_DF_MIN__ __DBL_MIN__",
            "#define __LIBGCC_DWARF_CIE_DATA_ALIGNMENT__ -8",
            "#define __LIBGCC_DWARF_FRAME_REGISTERS__ 17",
            "#define __LIBGCC_EH_FRAME_SECTION_NAME__ \".eh_frame\"",
            "#define __LIBGCC_EH_RETURN_STACKADJ_RTX__ 1",
            "#define __LIBGCC_EH_TABLES_CAN_BE_READ_ONLY__ 1",
            "#define __LIBGCC_HAS_DF_MODE__ 1",
            "#define __LIBGCC_HAS_HF_MODE__ 1",
            "#define __LIBGCC_HAS_SF_MODE__ 1",
            "#define __LIBGCC_HAS_TF_MODE__ 1",
            "#define __LIBGCC_HAS_XF_MODE__ 1",
            "#define __LIBGCC_HAVE_HWDBL__ 1",
            "#define __LIBGCC_HAVE_LIBATOMIC 0",
            "#define __LIBGCC_HF_EPSILON__ __FLT16_EPSILON__",
            "#define __LIBGCC_HF_EXCESS_PRECISION__ 1",
            "#define __LIBGCC_HF_FUNC_EXT__ f16",
            "#define __LIBGCC_HF_MANT_DIG__ 11",
            "#define __LIBGCC_HF_MAX__ __FLT16_MAX__",
            "#define __LIBGCC_HF_MIN__ __FLT16_MIN__",
            "#define __LIBGCC_INIT_ARRAY_SECTION_ASM_OP__ 1",
            "#define __LIBGCC_SF_EPSILON__ __FLT_EPSILON__",
            "#define __LIBGCC_SF_EXCESS_PRECISION__ 0",
            "#define __LIBGCC_SF_FUNC_EXT__ f",
            "#define __LIBGCC_SF_MANT_DIG__ 24",
            "#define __LIBGCC_SF_MAX__ __FLT_MAX__",
            "#define __LIBGCC_SF_MIN__ __FLT_MIN__",
            "#define __LIBGCC_STACK_GROWS_DOWNWARD__ 1",
            "#define __LIBGCC_STACK_POINTER_REGNUM__ 7",
            "#define __LIBGCC_TEXT_SECTION_ASM_OP__ \"\\t.text\"",
            "#define __LIBGCC_TF_EPSILON__ __FLT128_EPSILON__",
            "#define __LIBGCC_TF_EXCESS_PRECISION__ 0",
            "#define __LIBGCC_TF_FUNC_EXT__ f128",
            "#define __LIBGCC_TF_MANT_DIG__ 113",
            "#define __LIBGCC_TF_MAX__ __FLT128_MAX__",
            "#define __LIBGCC_TF_MIN__ __FLT128_MIN__",
            "#define __LIBGCC_TRAMPOLINE_SIZE__ 28",
            "#define __LIBGCC_VTABLE_USES_DESCRIPTORS__ 0",
            "#define __LIBGCC_XF_EPSILON__ __LDBL_EPSILON__",
            "#define __LIBGCC_XF_EXCESS_PRECISION__ 0",
            "#define __LIBGCC_XF_FUNC_EXT__ l",
            "#define __LIBGCC_XF_MANT_DIG__ 64",
            "#define __LIBGCC_XF_MAX__ __LDBL_MAX__",
            "#define __LIBGCC_XF_MIN__ __LDBL_MIN__",
        ];
        assert_eq!(libgcc("x86_64-unknown-linux-gnu"), gcc);
        // A BSD is the same machine and the same ELF.
        assert_eq!(libgcc("x86_64-unknown-netbsd"), gcc);
        assert_eq!(libgcc("x86_64-unknown-freebsd15.1"), gcc);
        // i386 differs in the word and so in what follows from it. Its arithmetic is SSE here, so
        // `float` and `double` are not computed wider, as in gcc with `-mfpmath=sse`.
        let i386 = libgcc("i486-unknown-netbsdelf");
        for line in [
            "#define __LIBGCC_BITINT_LIMB_WIDTH__ 32",
            "#define __LIBGCC_DWARF_CIE_DATA_ALIGNMENT__ -4",
            "#define __LIBGCC_TRAMPOLINE_SIZE__ 14",
            "#define __LIBGCC_SF_EXCESS_PRECISION__ 0",
            "#define __LIBGCC_XF_FUNC_EXT__ l",
        ] {
            assert!(i386.iter().any(|have| have == line), "no {line} in {i386:#?}");
        }
        // `long double` is the quad on AArch64, which takes the `l` suffix from `_Float128`.
        let arm = libgcc("aarch64-unknown-linux-gnu");
        for line in [
            "#define __LIBGCC_TF_FUNC_EXT__ l",
            "#define __LIBGCC_TF_EPSILON__ __LDBL_EPSILON__",
            "#define __LIBGCC_DWARF_FRAME_REGISTERS__ 97",
            "#define __LIBGCC_STACK_POINTER_REGNUM__ 31",
        ] {
            assert!(arm.iter().any(|have| have == line), "no {line} in {arm:#?}");
        }
        assert!(!arm.iter().any(|have| have.contains("_XF_")), "{arm:#?}");
        // Nothing without the flag.
        assert!(!set_for("x86_64-unknown-linux-gnu").contains("__LIBGCC_"));
    }

    /// The values are the ones gcc 16 gives on Arch Linux, for C and for assembly.
    #[test]
    fn the_stack_protector_and_cet_macros_have_gcc_s_values() {
        let x86 = TargetInfo::new("x86_64-unknown-linux-gnu".parse().expect("a triple"));
        let plain = built_in(&x86, &Predef::new());
        assert!(!plain.contains("__SSP"), "{plain}");
        assert!(!plain.contains("__CET__"), "{plain}");
        for (protector, line) in [
            (Protector::Buffers, "#define __SSP__ 1"),
            (Protector::All, "#define __SSP_ALL__ 2"),
            (Protector::Strong, "#define __SSP_STRONG__ 3"),
            (Protector::Explicit, "#define __SSP_EXPLICIT__ 4"),
        ] {
            for assembler in [false, true] {
                let text = built_in(&x86, &Predef { protector, assembler, ..Predef::new() });
                assert!(has(&text, line), "{protector}: {text}");
                assert_eq!(text.matches("__SSP").count(), 1, "{protector}: {text}");
            }
        }
        for (control, bits) in [
            (Control::Branch, "1"),
            (Control::Return, "2"),
            (Control::Full, "3"),
            (Control::Check, "8"),
        ] {
            for triple in ["x86_64-unknown-linux-gnu", "i686-unknown-linux-gnu"] {
                let target = TargetInfo::new(triple.parse().expect("a triple"));
                for assembler in [false, true] {
                    let text = built_in(&target, &Predef { control, assembler, ..Predef::new() });
                    assert!(has(&text, &format!("#define __CET__ {bits}")), "{triple}: {text}");
                }
            }
        }
        let arm = TargetInfo::new("aarch64-unknown-linux-gnu".parse().expect("a triple"));
        let text = built_in(&arm, &Predef { control: Control::Full, ..Predef::new() });
        assert!(!text.contains("__CET__"), "gcc defines it only on x86");
    }

    #[test]
    fn the_arm_wchar_size_is_the_target_s_wchar_size() {
        // The ACLE macro and `__SIZEOF_WCHAR_T__` are the same fact said twice, and on Windows
        // both are two, which is what clang for aarch64-w64-mingw32 defines.
        for (triple, size) in [
            ("aarch64-unknown-linux-gnu", 4),
            ("aarch64-apple-darwin", 4),
            ("aarch64-pc-windows-gnu", 2),
            ("aarch64-pc-windows-msvc", 2),
        ] {
            let set = set_for(triple);
            assert!(has(&set, &format!("#define __ARM_SIZEOF_WCHAR_T {size}")), "{triple}");
            assert!(has(&set, &format!("#define __SIZEOF_WCHAR_T__ {size}")), "{triple}");
        }
        // And `char` is signed on Windows on this machine, as on Apple's, and unsigned on Linux.
        assert!(!set_for("aarch64-pc-windows-gnu").contains("__CHAR_UNSIGNED__"));
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
    fn windows_is_the_one_target_where_the_two_middle_fast_types_differ_from_each_other() {
        // mingw's `stdint.h` declares `int_fast16_t` a `short` and `int_fast32_t` an `int`, and
        // x86_64-w64-mingw32-gcc says the same, so this is the one platform where the pair does
        // not share an answer.
        let windows = set_for("x86_64-pc-windows-gnu");
        assert!(has(&windows, "#define __INT_FAST16_TYPE__ short int"));
        assert!(has(&windows, "#define __UINT_FAST16_TYPE__ short unsigned int"));
        assert!(has(&windows, "#define __INT_FAST16_MAX__ 0x7fff"));
        assert!(has(&windows, "#define __UINT_FAST16_MAX__ 0xffff"));
        assert!(has(&windows, "#define __INT_FAST16_WIDTH__ 16"));
        assert!(has(&windows, "#define __INT_FAST32_TYPE__ int"));
        assert!(has(&windows, "#define __UINT_FAST32_TYPE__ unsigned int"));
        assert!(has(&windows, "#define __INT_FAST32_WIDTH__ 32"));
        // The two ends of the family are the same as everywhere.
        assert!(has(&windows, "#define __INT_FAST8_TYPE__ signed char"));
        assert!(has(&windows, "#define __INT_FAST64_TYPE__ long long int"));
    }

    #[test]
    fn windows_answers_to_every_name_gcc_gives_it() {
        // The mingw tree reads more than one spelling of the platform and a missing one is a
        // declaration that quietly is not there: `winuser.h` guards `EndTask` with `#ifdef
        // WINNT` and `rpcdcep.h` guards six `I_Rpc` declarations with `#ifndef WINNT`.
        let windows = set_for("x86_64-pc-windows-gnu");
        let every = "_WIN32 __WIN32 __WIN32__ __WINNT __WINNT__ __MINGW32__ \
                     _WIN64 __WIN64 __WIN64__ __MINGW64__ __MSVCRT__ __SEH__ WIN32 WIN64 WINNT";
        for name in every.split_whitespace() {
            assert!(has(&windows, &format!("#define {name} 1")), "no {name}");
        }
        assert!(has(&windows, "#define _INTEGRAL_MAX_BITS 64"));
    }

    #[test]
    fn a_thirty_two_bit_windows_is_not_told_its_pointer_is_sixty_four_bits_wide() {
        // `_WIN64` is about the pointer rather than the processor, and i686-w64-mingw32-gcc
        // defines neither it nor `__MINGW64__`, nor `__SEH__`, since the unwind records this
        // compiler writes are the sixty four bit format and a thirty two bit Windows unwinds
        // some other way. It does define `_X86_`, which the headers read for the processor.
        let windows = set_for_tuple("i686-w64-windows-gnu");
        assert!(has(&windows, "#define _WIN32 1"));
        assert!(has(&windows, "#define __MINGW32__ 1"));
        assert!(has(&windows, "#define _X86_ 1"));
        assert!(!has(&set_for("x86_64-pc-windows-gnu"), "#define _X86_ 1"));
        for name in ["_WIN64", "__WIN64", "__WIN64__", "__MINGW64__", "__SEH__", "WIN64"] {
            assert!(!has(&windows, &format!("#define {name} 1")), "{name} on a 32 bit target");
        }
    }

    #[test]
    fn an_msvc_row_says_what_clang_says_for_it_and_none_of_what_mingw_says() {
        let msvc = set_for("x86_64-pc-windows-msvc");
        for line in [
            "#define _WIN32 1",
            "#define _WIN64 1",
            "#define _M_X64 100",
            "#define _M_AMD64 100",
            "#define _MSC_VER 1940",
            "#define _MSC_FULL_VER 194000000",
            "#define _MSC_BUILD 1",
            "#define _MSC_EXTENSIONS 1",
            "#define _INTEGRAL_MAX_BITS 64",
            "#define __x86_64__ 1",
        ] {
            assert!(has(&msvc, line), "no {line}");
        }
        // The names that send the SDK headers down a path written for another compiler or
        // another runtime, and the macros that would hide the keywords.
        for name in [
            "__GNUC__",
            "__GNUC_MINOR__",
            "__GNUC_STDC_INLINE__",
            "__GXX_ABI_VERSION",
            "__clang__",
            "__MINGW32__",
            "__MINGW64__",
            "__MSVCRT__",
            "__SEH__",
            "WIN32",
            "__WIN32__",
            "__cdecl",
            "_cdecl",
            "__declspec(x)",
            "_M_ARM64",
            "_M_IX86",
        ] {
            assert!(!msvc.contains(&format!("#define {name} ")), "{name} on msvc");
        }
        let msvc32 = set_for_tuple("i686-pc-windows-msvc");
        assert!(has(&msvc32, "#define _M_IX86 600"));
        assert!(!has(&msvc32, "#define _WIN64 1"));
        assert!(!has(&msvc32, "#define _X86_ 1"), "the mingw spelling is not Microsoft's");
    }

    #[test]
    fn the_mingw_row_keeps_its_gcc_set_beside_the_msvc_one() {
        let gnu = set_for("x86_64-pc-windows-gnu");
        assert!(has(&gnu, "#define __GNUC__ 16"));
        assert!(has(&gnu, "#define __cdecl __attribute__((__cdecl__))"));
        assert!(has(&gnu, "#define __declspec(x) __attribute__((x))"));
        assert!(!gnu.contains("_MSC_VER"));
        assert!(!gnu.contains("_M_X64"));
    }

    #[test]
    fn an_msvc_row_on_arm64_names_that_architecture() {
        let msvc = set_for("aarch64-pc-windows-msvc");
        assert!(has(&msvc, "#define _M_ARM64 1"));
        assert!(!msvc.contains("_M_X64"));
        assert!(has(&msvc, "#define _WIN64 1"));
    }

    #[test]
    fn the_msvc_version_and_the_gcc_claim_are_both_flags() {
        let target = TargetInfo::new("x86_64-pc-windows-msvc".parse().expect("a triple"));
        let mut opts = Predef::new();
        opts.msc = "19.29.30133".parse().expect("a version");
        opts.gnuc_given = true;
        opts.ms_extensions = Some(false);
        let set = built_in(&target, &opts);
        assert!(has(&set, "#define _MSC_VER 1929"));
        assert!(has(&set, "#define _MSC_FULL_VER 192930133"));
        assert!(has(&set, "#define __GNUC__ 16"), "-fgnuc-version= puts it back, as in clang");
        assert!(!set.contains("_MSC_EXTENSIONS"));
    }

    #[test]
    fn the_c_runtime_an_msvc_row_links_against_is_in_two_macros() {
        // `/MT` by default, which is `_MT` alone, and `/MD` adds `_DLL`, which is what the
        // headers read to declare the runtime's functions as imported.
        let target = TargetInfo::new("x86_64-pc-windows-msvc".parse().expect("a triple"));
        let statically = built_in(&target, &Predef::new());
        assert!(has(&statically, "#define _MT 1"));
        assert!(!statically.contains("#define _DLL "));
        let opts = Predef { ms_dll_runtime: true, ..Predef::new() };
        let dll = built_in(&target, &opts);
        assert!(has(&dll, "#define _MT 1"));
        assert!(has(&dll, "#define _DLL 1"));
        // And neither on the mingw-w64 row, whose runtime is not chosen this way.
        let gnu =
            built_in(&TargetInfo::new("x86_64-pc-windows-gnu".parse().expect("a triple")), &opts);
        assert!(!gnu.contains("#define _MT ") && !gnu.contains("#define _DLL "));
    }

    #[test]
    fn the_three_unreserved_windows_names_need_the_gnu_dialect() {
        // `WIN32`, `WIN64` and `WINNT` are in the user's namespace, so gcc drops all three under
        // `-std=c11` and keeps the underscored ones. Windows code tests them anyway, the same
        // way portable Unix code still tests `linux`.
        let triple: Triple = "x86_64-pc-windows-gnu".parse().expect("a triple");
        let mut opts = Predef::new();
        opts.gnu_extensions = false;
        let strict = built_in(&TargetInfo::new(triple), &opts);
        for name in ["WIN32", "WIN64", "WINNT"] {
            assert!(!has(&strict, &format!("#define {name} 1")), "{name} under -std=c11");
        }
        assert!(has(&strict, "#define _WIN32 1"));
        assert!(has(&strict, "#define __WINNT__ 1"));
    }

    #[test]
    fn a_wasm_target_says_wasm_and_its_wasi_preview_and_nothing_unix() {
        for (triple, preview) in [
            ("wasm32-wasip1", Some(1)),
            ("wasm32-wasip2", Some(2)),
            ("wasm32-wasip3", Some(3)),
            ("wasm32-unknown-unknown", None),
        ] {
            let text = set_for(triple);
            for name in ["__wasm", "__wasm__", "__wasm32", "__wasm32__"] {
                assert!(has(&text, &format!("#define {name} 1")), "{triple}: {name}");
            }
            assert_eq!(has(&text, "#define __wasi__ 1"), preview.is_some(), "{triple}");
            for n in 1..=3 {
                let line = format!("#define __wasip{n}__ 1");
                assert_eq!(has(&text, &line), preview == Some(n), "{triple}: {line}");
            }
            // A wasm object is not ELF, the target is not Unix, and rucc writes no position
            // independent wasm.
            for name in [
                "__ELF__",
                "__unix__",
                "__unix",
                "unix",
                "__linux__",
                "__pic__",
                "__PIC__",
                "__pie__",
                "__PIE__",
            ] {
                assert!(
                    !text.lines().any(|l| l.starts_with(&format!("#define {name} "))),
                    "{triple}: {name}"
                );
            }
            assert!(
                !has(&text, "#define __x86_64__ 1") && !has(&text, "#define __aarch64__ 1"),
                "{triple}"
            );
        }
        // A freestanding ELF target keeps `__ELF__`.
        assert!(has(&set_for("x86_64-unknown-none"), "#define __ELF__ 1"));
    }

    /// tamnd/rucc#4088. gcc names the two address spaces and the x87 type on x86 in both widths,
    /// and NetBSD's gcc names the processor it tunes for until a flag names another. The values
    /// are what the reference gcc of FreeBSD 15.1 and NetBSD 11 give, with and without `-m32`.
    #[test]
    fn x86_names_its_address_spaces_its_x87_type_and_the_netbsd_tuning() {
        for (triple, float80) in [
            ("x86_64-unknown-freebsd", "16"),
            ("i686-unknown-linux-gnu", "12"),
            ("x86_64-unknown-netbsd", "16"),
            ("i486--netbsdelf", "12"),
        ] {
            let set = set_for(triple);
            assert!(has(&set, "#define __SEG_FS 1"), "{triple}");
            assert!(has(&set, "#define __SEG_GS 1"), "{triple}");
            assert!(has(&set, &format!("#define __SIZEOF_FLOAT80__ {float80}")), "{triple}");
            assert!(has(&set, "#define __GCC_ASM_FLAG_OUTPUTS__ 1"), "{triple}");
            let netbsd = triple.contains("netbsd");
            assert_eq!(has(&set, "#define __tune_nocona__ 1"), netbsd, "{triple}");
        }
        let triple: Triple = "x86_64-unknown-netbsd".parse().unwrap();
        let tuned = Predef { tuned: true, ..Predef::new() };
        assert!(!built_in(&TargetInfo::new(triple), &tuned).contains("__tune_nocona__"));
        // clang has its own list, and an aarch64 has no x87 and no segments.
        for triple in ["x86_64-apple-darwin", "x86_64-pc-windows-msvc", "aarch64-unknown-netbsd"] {
            let set = set_for(triple);
            assert!(!set.contains("__SEG_FS") && !set.contains("__SIZEOF_FLOAT80__"), "{triple}");
            assert!(!set.contains("__tune_"), "{triple}");
        }
    }

    /// tamnd/rucc#4251. A 32 bit unit says the processor it is built for and what that processor
    /// has, as gcc does. The amd64 gcc of a BSD builds `-m32` code for the K8, and the boot code's
    /// `-march=i386` has no extensions and no `cmpxchg`. The values are what the reference gcc of
    /// FreeBSD 15.1 and NetBSD 11 give, and gcc 13.3 for the i486.
    #[test]
    fn a_32_bit_unit_names_its_processor_and_what_it_has() {
        let built = |triple: &str, name: &str, tune: bool| {
            let triple: Triple = triple.parse().unwrap();
            let processor = Processor::named(name).unwrap();
            let tune = if tune { processor.tune } else { &[] };
            let isa = processor.isa().union(Isa::of(&["sahf"]));
            let opts =
                Predef { processor: Some(processor), tune, isa, tuned: true, ..Predef::new() };
            built_in(&TargetInfo::new(triple), &opts)
        };
        let k8 = built("i386-unknown-freebsd15.1", "x86-64", false);
        for name in
            ["__k8", "__k8__", "__MMX__", "__SSE__", "__SSE2__", "__FXSR__", "__LAHF_SAHF__"]
        {
            assert!(has(&k8, &format!("#define {name} 1")), "{name}: {k8}");
        }
        for name in ["__i686", "__pentium4", "__tune_", "__GCC_HAVE_DWARF2_CFI_ASM"] {
            assert!(!k8.contains(name), "{name}: {k8}");
        }
        assert!(has(&k8, "#define __GCC_HAVE_SYNC_COMPARE_AND_SWAP_8 1"), "{k8}");
        assert!(has(&k8, "#define __GCC_ATOMIC_LLONG_LOCK_FREE 2"), "{k8}");
        let boot = built("i486--netbsdelf", "i386", true);
        assert!(has(&boot, "#define __tune_i386__ 1"), "{boot}");
        for name in ["__MMX__", "__SSE__", "__SSE2__", "__FXSR__", "__i486", "__i686", "__k8"] {
            assert!(!boot.contains(name), "{name}: {boot}");
        }
        for name in ["BOOL", "CHAR", "SHORT", "INT", "LONG", "LLONG", "POINTER", "WCHAR_T"] {
            let line = format!("#define __GCC_ATOMIC_{name}_LOCK_FREE 1");
            assert!(has(&boot, &line), "{line}: {boot}");
        }
        assert!(!boot.contains("__GCC_HAVE_SYNC_COMPARE_AND_SWAP"), "{boot}");
        // NetBSD's own i386 gcc is configured for the i486, which has `cmpxchg` up to the word.
        let i486 = built("i486--netbsdelf", "i486", false);
        assert!(has(&i486, "#define __i486__ 1") && has(&i486, "#define __i486 1"), "{i486}");
        assert!(has(&i486, "#define __GCC_ATOMIC_INT_LOCK_FREE 2"), "{i486}");
        assert!(has(&i486, "#define __GCC_ATOMIC_LLONG_LOCK_FREE 1"), "{i486}");
        assert!(has(&i486, "#define __GCC_HAVE_SYNC_COMPARE_AND_SWAP_4 1"), "{i486}");
        assert!(!i486.contains("__GCC_HAVE_SYNC_COMPARE_AND_SWAP_8"), "{i486}");
        // NetBSD's gcc keeps the unwind tables under `-m32`, and FreeBSD's does not.
        let netbsd = built("i486--netbsdelf", "x86-64", false);
        assert!(has(&netbsd, "#define __GCC_HAVE_DWARF2_CFI_ASM 1"), "{netbsd}");
        // With no processor the unit is the Pentium 4 the backend builds for, as before.
        let linux = set_for("i686-unknown-linux-gnu");
        for name in ["__i686", "__pentium4", "__SSE2__", "__GCC_HAVE_SYNC_COMPARE_AND_SWAP_8"] {
            assert!(linux.contains(name), "{name}: {linux}");
        }
    }
}
