//! Real libraries, built instrumented, linked, run against a real workload, and held to their
//! answers.
//!
//! Design: `spec/safe-memory/14-verification.md` section 14.9 and the seventh box of
//! tamnd/rucc#1307.
//!
//! Everything in `tests/safety` is a few lines of C written to provoke one judgement, which is the
//! right shape for asking whether a check fires. It is the wrong shape for asking whether a real
//! library survives the monitor, and the two are different questions. tamnd/rucc#1307 is what the
//! gap between them costs: every interposed library write recorded the init plane and said nothing
//! on the type plane, which no case in the suite could see, and an instrumented SQLite aborted
//! within a few hundred calls because its lookaside allocator hands the same block out as three
//! different structures. Three more holes of the same shape have been found since, and every one
//! of them was found by running a real library by hand.
//!
//! So it stops being by hand. A library that recycles its own storage, walks its own trees and
//! answers questions whose answers are arithmetic is a thing the suite cannot imitate and does not
//! have to: the sources compile, rucc builds them, and the answers are either right or they are
//! not.
//!
//! # Why there is a table
//!
//! One library is an anecdote. SQLite is a single enormous translation unit that recycles small
//! blocks through a lookaside allocator, and it leans on the type plane harder than anything else
//! we have. zlib is fifteen small ones that take a few large buffers at the start and then index
//! them with pointers held inside the caller's own structure, which leans on the capability
//! instead, and it walks into the init plane the first time it slides its hash table. Lua is
//! neither: every value it moves is a tagged union whose payload is sometimes a pointer and
//! sometimes an integer of the same width, its collector reaches every live object by walking those
//! unions, and its errors leave through longjmp from the middle of a C stack the interpreter built.
//! brotli is the same job as zlib written a different way: a static dictionary of a hundred and
//! twenty thousand words, a ring buffer it grows as it learns how much it needs, and a distance that
//! is a number selecting storage from one of several places rather than a pointer to it. zstd keeps
//! several tables of positions over one buffer at once and switches between them by strategy, so the
//! same storage is indexed three ways in one compression, and its dictionary builder sorts a suffix
//! array, which is the only code here that sorts pointers into a buffer instead of walking them.
//! libwebp is the first one whose subject is a rectangle rather than a run of bytes: it indexes its
//! storage by two numbers multiplied by a stride the caller chose, and it picks which version of
//! every inner loop to run by asking the processor what it is at startup and filling in a table of
//! function pointers, so nearly every call into its pixel code goes through a pointer that was
//! stored once and is read from everywhere after. libjpeg-turbo cuts its planes into eight by eight
//! blocks and keeps every buffer as an array of row pointers out of pools it frees all at once, and
//! it builds most of its codec three times by compiling one file under three settings of a macro,
//! so one source file is three sets of functions over three sample widths. None of the seven
//! reaches the others' paths. The rows here are the projects and the code below is the same for all
//! of them, which is what makes adding the next one a paragraph of data.
//!
//! # Why the sources are not in the tree
//!
//! They are somebody else's C and they have versions. Vendoring them would put a copy of SQLite in
//! a compiler's history forever and make every bump of it a commit here, and the checks do not care
//! which version they get: the workloads are ordinary use and the answers are arithmetic the
//! workload does itself. So the sources are looked for and the check says what to fetch when they
//! are not there, which is the same bargain `xtask/src/real_libc.rs` makes about a glibc abilist
//! and for the same reason.
//!
//! # What it is checking
//!
//! That each program links, runs, and prints the answers its own arithmetic says it should, at
//! `-O0` and at `-O2`. Both levels, because the two failures are different: at `-O0` a wrong answer
//! is the instrumentation or the runtime, and at `-O2` it is either of those or a check the
//! optimizer removed that was holding something up.
//!
//! And that the monitor said exactly what the row says it should. A report the row does not list is
//! a failure, because these workloads do nothing wrong, so an unlisted report is a false positive
//! by construction. A listed report that did not arrive is a failure too, because the row is a
//! claim that the library really does do that and the day it stops being true is a day somebody
//! should look.
//!
//! A row may be green on the first of those and waiting on the second, which is what
//! [`Project::pending`] is and what zstd is currently doing. It says which question its reports are
//! waiting on, the check prints that beside the row rather than swallowing it, and nothing else in
//! the table gets quieter.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::runner::{Runner, TRIPLE};
use crate::safety::{self, BANNER};
use crate::{Error, Result, cost, indent, root, target_dir};

/// What a program has to say before its answers count.
///
/// The workloads check their own arithmetic and print this when all of it came out, which keeps the
/// expected numbers in the C files beside the work that produces them rather than here, where they
/// would be a second copy to keep in step.
const CORRECT: &str = "all answers correct";

/// The levels everything is built and run at.
const LEVELS: [&str; 2] = ["-O0", "-O2"];

/// What the link needs beyond the objects and the runtime archive.
///
/// The runtime's own three, which SQLite's configure script asks for as well and which are a
/// superset of what Lua's makefile asks for, so one list covers every row so far and will cover most
/// of the next ones.
pub(crate) const LIBRARIES: [&str; 3] = ["-lpthread", "-lm", "-ldl"];

/// A report a library earns honestly, so that the check can tell one from a false positive.
///
/// There is no symbol in a report and the address in one is different every run, so what is matched
/// is the judgement and the width of the access. That is coarse, and it is the same coarseness the
/// safety suite's own rows have. What carries the meaning is `why`, which names the site for the
/// person reading this rather than for the code.
struct Known {
    /// The judgement number, as `J1` is written in a report.
    judgement: u32,
    /// The width of the access, in bytes.
    bytes: u32,
    /// The site, and why the library is entitled to it.
    why: &'static str,
}

/// The headers a project's own build would write, and the values it would write into them.
///
/// The project's templates are filled in here the way cmake's `configure_file` fills them in, which
/// is two rules. `@NAME@` anywhere in a line becomes the value the row gives `NAME`, and a line that
/// says `#cmakedefine NAME rest` becomes `#define NAME rest` when the row gives `NAME` a value and
/// a commented out `#undef` when it does not. `#cmakedefine01 NAME` is the same question answered
/// with a `1` or a `0`. That is all three of libjpeg-turbo's templates ask for. `${NAME}` is the
/// third form cmake knows and nothing here uses it, so it is left alone rather than half supported.
///
/// Values in the row rather than the filled in headers in the tree, for the reason the sources are
/// not in the tree. The templates are the project's and they change between versions, so a copy of
/// what cmake wrote for one version is a file that quietly goes stale the first time somebody
/// points the variable at the next one. Filling in whatever templates the tarball has keeps the
/// project's text the project's, and what the row owns is the dozen answers cmake would have found
/// by probing a Linux machine.
///
/// One rule is stricter than cmake's. A template that asks for `@NAME@` when the row has no `NAME`
/// is an error here, where cmake writes an empty string. An empty string in a header compiles into
/// something wrong far more often than it fails to compile, and the first anyone would hear of it
/// is a library that answers wrongly.
#[derive(Clone, Copy)]
pub(crate) struct Configure {
    /// The templates, relative to the source directory. Each is written under its own file name
    /// with the `.in` taken off, into one directory that goes on the include path.
    pub(crate) templates: &'static [&'static str],
    /// What cmake would have worked out on an x86-64 Linux machine, as a name and a value. A name
    /// missing from this list is a feature that is off.
    pub(crate) settings: &'static [(&'static str, &'static str)],
}

/// Fills in each of a project's templates and says which directory to put on the include path.
///
/// None when the project has no templates, which is every row but one, so that the two builds can
/// ask unconditionally.
pub(crate) fn configure(project: &Project, source: &Path, work: &Path) -> Result<Option<PathBuf>> {
    let Some(configure) = project.configure else {
        return Ok(None);
    };
    let out = work.join("configured");
    std::fs::create_dir_all(&out)
        .map_err(|e| Error::Io(format!("could not make {}: {e}", out.display())))?;
    for template in configure.templates {
        let from = source.join(template);
        let text = std::fs::read_to_string(&from)
            .map_err(|e| Error::Io(format!("could not read {}: {e}", from.display())))?;
        let filled = fill(&text, configure.settings).map_err(|name| {
            Error::Io(format!(
                "{} asks for {name} and the {} row does not say what it is",
                from.display(),
                project.name
            ))
        })?;
        let name = Path::new(template).file_name().and_then(|n| n.to_str()).unwrap_or(template);
        let to = out.join(name.trim_end_matches(".in"));
        std::fs::write(&to, filled)
            .map_err(|e| Error::Io(format!("could not write {}: {e}", to.display())))?;
    }
    Ok(Some(out))
}

/// One template filled in, or the first name it asks for that the settings do not have.
fn fill(text: &str, settings: &[(&str, &str)]) -> std::result::Result<String, String> {
    let value = |name: &str| settings.iter().find(|(n, _)| *n == name).map(|(_, v)| *v);
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let line = match cmakedefine(line) {
            Some((true, name, _)) => {
                format!("#define {name} {}", if value(name).is_some() { 1 } else { 0 })
            }
            Some((false, name, rest)) => match value(name) {
                Some(_) => format!("#define {name}{rest}"),
                None => format!("/* #undef {name} */"),
            },
            None => line.to_owned(),
        };
        out.push_str(&substitute(&line, &value)?);
        out.push('\n');
    }
    Ok(out)
}

/// A `#cmakedefine` line taken apart, as whether it is the `01` form, the name, and everything after
/// the name.
///
/// cmake allows space between the `#` and the word, as the preprocessor does, so this does too.
fn cmakedefine(line: &str) -> Option<(bool, &str, &str)> {
    let rest = line.trim_start().strip_prefix('#')?.trim_start();
    let (zero_one, rest) = match rest.strip_prefix("cmakedefine01") {
        Some(rest) => (true, rest),
        None => (false, rest.strip_prefix("cmakedefine")?),
    };
    if !rest.starts_with([' ', '\t']) {
        return None;
    }
    let rest = rest.trim_start();
    let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    Some((zero_one, &rest[..end], &rest[end..]))
}

/// Every `@NAME@` in a line replaced by its value, or the first name with none.
///
/// A name is what a C identifier is, so an `@` that is not followed by one and closed by another `@`
/// is left as it is, which is what keeps an address in a comment out of this.
fn substitute<'a>(
    line: &str,
    value: &impl Fn(&str) -> Option<&'a str>,
) -> std::result::Result<String, String> {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = rest.find('@') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let end =
            after.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(after.len());
        if end > 0 && after[end..].starts_with('@') {
            let name = &after[..end];
            out.push_str(value(name).ok_or_else(|| name.to_owned())?);
            rest = &after[end + 1..];
        } else {
            out.push('@');
            rest = after;
        }
    }
    out.push_str(rest);
    Ok(out)
}

/// A library, everything needed to build it, and everything the monitor is expected to say.
pub(crate) struct Project {
    /// The name, which is the case name in the report, the directory the build goes in, and the
    /// directory under `tests` the workload lives in.
    pub(crate) name: &'static str,
    /// The variable somebody points at the sources, which may name the directory or the file inside
    /// it that [`Project::marker`] gives.
    pub(crate) variable: &'static str,
    /// The file that says an unpacked directory really is this project.
    marker: &'static str,
    /// Directory names the project's own tarball unpacks to, under the two places a person is
    /// likely to have put it.
    usual: &'static [&'static str],
    /// The C files to build, relative to the source directory.
    pub(crate) sources: &'static [&'static str],
    /// Anything else to put on the include path, relative to the source directory.
    ///
    /// The source directory itself is always searched, which is all the first rows needed. A project
    /// that keeps its public headers apart from its C, which is most of the larger ones, names the
    /// directory here and the workload gets the same path the library does.
    pub(crate) includes: &'static [&'static str],
    /// What the project's configure script would have defined on a Linux machine.
    pub(crate) defines: &'static [&'static str],
    /// The headers the project's own build writes before it compiles anything, when it does.
    ///
    /// Most of the rows need nothing here, because a project that ships a `config.h` or never asks
    /// for one hands over C that compiles as it comes. libjpeg-turbo is the one that does not: its
    /// tarball has `jconfig.h.in` and two more like it, and cmake fills them in, so a source tree
    /// nobody has run cmake over does not compile. See [`Configure`] for why the answer is a few
    /// values in the row rather than a copy of the filled in headers.
    pub(crate) configure: Option<Configure>,
    /// Every report the monitor should make, and nothing else.
    known: &'static [Known],
    /// Why the monitor's reports are not held against [`Project::known`] yet, when they are not.
    ///
    /// A row is normally green on both halves, which is that the program gets its answers right and
    /// that the monitor said exactly what the row says it would. A row can be green on the first
    /// and blocked on the second, and zstd is the case that made this field: the model refuses a
    /// derivation that library performs as its ordinary way of working, so the reports are neither
    /// false positives nor a list somebody can write down, they are one open question arriving a
    /// few hundred thousand times. Listing them one by one would say nothing and would break on the
    /// next release, and dropping the row would throw away the half that does work, which is that
    /// thirty translation units compile at both levels, link, run and answer correctly.
    ///
    /// So the row says which question it is waiting on, the check prints that beside the row rather
    /// than swallowing it, and the field goes away when the question is answered. Nothing else in
    /// the table gets quieter, because this is per row.
    pending: Option<&'static str>,
}

/// The projects, in the order they are run.
///
/// SQLite first because it is the one four holes were found with, and because nine megabytes of C
/// compiled twice is most of the time this check takes either way.
pub(crate) const PROJECTS: &[Project] = &[
    Project {
        name: "sqlite",
        variable: "RUCC_SQLITE_AMALGAMATION",
        marker: "sqlite3.c",
        usual: &["sqlite-autoconf", "sqlite"],
        sources: &["sqlite3.c"],
        includes: &[],
        defines: &[],
        configure: None,
        known: &[],
        pending: None,
    },
    Project {
        name: "zlib",
        variable: "RUCC_ZLIB_SOURCE",
        marker: "zlib.h",
        usual: &["zlib"],
        sources: &[
            "adler32.c",
            "compress.c",
            "crc32.c",
            "deflate.c",
            "gzclose.c",
            "gzlib.c",
            "gzread.c",
            "gzwrite.c",
            "infback.c",
            "inffast.c",
            "inflate.c",
            "inftrees.c",
            "trees.c",
            "uncompr.c",
            "zutil.c",
        ],
        includes: &[],
        // What zlib's configure writes on any Linux machine. Without it the three gzip files do not
        // compile at all, because zlib only reaches for unistd.h when it has been told the header
        // is there.
        defines: &["HAVE_UNISTD_H=1"],
        configure: None,
        known: &[Known {
            judgement: 1,
            bytes: 2,
            why: "slide_hash in deflate.c reads every entry of s->prev before anything has \
                  written it. The allocation is a plain malloc, the loop reads each two byte entry \
                  and writes a value derived from it back, and zlib's own comment beside the loop \
                  says the value is garbage for any position not on a hash chain and will never be \
                  used. That is true of what the value is used for and it is still a read of \
                  storage nothing initialised, so the init plane is right to say so. It is one of \
                  the four classes tamnd/rucc#431 is about and it is the first one a second \
                  project produced.",
        }],
        pending: None,
    },
    Project {
        name: "lua",
        variable: "RUCC_LUA_SOURCE",
        marker: "lua.h",
        usual: &["lua"],
        sources: &[
            "lapi.c",
            "lauxlib.c",
            "lbaselib.c",
            "lcode.c",
            "lcorolib.c",
            "lctype.c",
            "ldblib.c",
            "ldebug.c",
            "ldo.c",
            "ldump.c",
            "lfunc.c",
            "lgc.c",
            "linit.c",
            "liolib.c",
            "llex.c",
            "lmathlib.c",
            "lmem.c",
            "loadlib.c",
            "lobject.c",
            "lopcodes.c",
            "loslib.c",
            "lparser.c",
            "lstate.c",
            "lstring.c",
            "lstrlib.c",
            "ltable.c",
            "ltablib.c",
            "ltm.c",
            "lundump.c",
            "lutf8lib.c",
            "lvm.c",
            "lzio.c",
        ],
        includes: &[],
        // The one thing Lua's own makefile passes on this platform. It turns on the POSIX bits of
        // the io and os libraries and the dlopen path in loadlib.c, all of which are library code
        // this ought to be running rather than stubs. The two files with a main in them, lua.c and
        // luac.c, are not in the list above, and they are the only ones that would have wanted
        // readline.
        defines: &["LUA_USE_LINUX"],
        configure: None,
        known: &[],
        pending: None,
    },
    Project {
        name: "brotli",
        variable: "RUCC_BROTLI_SOURCE",
        marker: "c/include/brotli/decode.h",
        usual: &["brotli"],
        sources: &[
            "c/common/constants.c",
            "c/common/context.c",
            "c/common/dictionary.c",
            "c/common/platform.c",
            "c/common/shared_dictionary.c",
            "c/common/transform.c",
            "c/dec/bit_reader.c",
            "c/dec/decode.c",
            "c/dec/huffman.c",
            "c/dec/state.c",
            "c/enc/backward_references.c",
            "c/enc/backward_references_hq.c",
            "c/enc/bit_cost.c",
            "c/enc/block_splitter.c",
            "c/enc/brotli_bit_stream.c",
            "c/enc/cluster.c",
            "c/enc/command.c",
            "c/enc/compound_dictionary.c",
            "c/enc/compress_fragment.c",
            "c/enc/compress_fragment_two_pass.c",
            "c/enc/dictionary_hash.c",
            "c/enc/encode.c",
            "c/enc/encoder_dict.c",
            "c/enc/entropy_encode.c",
            "c/enc/fast_log.c",
            "c/enc/histogram.c",
            "c/enc/literal_cost.c",
            "c/enc/memory.c",
            "c/enc/metablock.c",
            "c/enc/static_dict.c",
            "c/enc/utf8_util.c",
        ],
        // brotli's public headers are the only ones outside its C, and both the library and the
        // workload reach them by the same spelling, so both get the same flag. The one file with a
        // main in it, c/tools/brotli.c, is not in the list above.
        includes: &["c/include"],
        defines: &[],
        configure: None,
        known: &[],
        pending: None,
    },
    Project {
        name: "zstd",
        variable: "RUCC_ZSTD_SOURCE",
        marker: "lib/zstd.h",
        usual: &["zstd"],
        sources: &[
            "lib/common/debug.c",
            "lib/common/entropy_common.c",
            "lib/common/error_private.c",
            "lib/common/fse_decompress.c",
            "lib/common/pool.c",
            "lib/common/threading.c",
            "lib/common/xxhash.c",
            "lib/common/zstd_common.c",
            "lib/compress/fse_compress.c",
            "lib/compress/hist.c",
            "lib/compress/huf_compress.c",
            "lib/compress/zstd_compress.c",
            "lib/compress/zstd_compress_literals.c",
            "lib/compress/zstd_compress_sequences.c",
            "lib/compress/zstd_compress_superblock.c",
            "lib/compress/zstd_double_fast.c",
            "lib/compress/zstd_fast.c",
            "lib/compress/zstd_lazy.c",
            "lib/compress/zstd_ldm.c",
            "lib/compress/zstd_opt.c",
            "lib/compress/zstd_preSplit.c",
            "lib/compress/zstdmt_compress.c",
            "lib/decompress/huf_decompress.c",
            "lib/decompress/zstd_ddict.c",
            "lib/decompress/zstd_decompress.c",
            "lib/decompress/zstd_decompress_block.c",
            "lib/dictBuilder/cover.c",
            "lib/dictBuilder/divsufsort.c",
            "lib/dictBuilder/fastcover.c",
            "lib/dictBuilder/zdict.c",
        ],
        // The list zstd's own makefile passes, less the legacy formats, which are old decoders
        // nobody builds unless they have old files.
        includes: &["lib", "lib/common", "lib/compress", "lib/decompress", "lib/dictBuilder"],
        // The one thing zstd's build decides by looking at the machine rather than at the
        // platform. Its Huffman decoder ships a hand written amd64 loop in a .S file, this table
        // compiles C and nothing else, and the flag is zstd's own way of saying to use the C the
        // loop replaces. Which is the right thing to measure here anyway, since assembly nobody
        // compiled is assembly the monitor has nothing to say about.
        //
        // There was a second one here for a while, and taking it back out is the point of it.
        // zstd's four tracing hooks are declared `__attribute__((weak))` and defined by nobody,
        // which is how a library offers a hook a profiler may fill in, and this compiler dropped
        // the attribute, so the four names arrived at the linker as ordinary undefined symbols and
        // thirty files would not link. `ZSTD_TRACE=0` is zstd's own way of saying to compile
        // without the hooks and stood in for the attribute until tamnd/rucc#1414 was done. The row
        // now builds what zstd builds.
        //
        // The other two are a declared exemption in the sense of section 12.6 of
        // spec/safe-memory/12-corpus-and-evidence.md, bucket five, and they are written here
        // rather than as markers in zstd's source because zstd already has the switch. On any
        // GCC-like compiler `lib/common/mem.h` and the bundled `xxhash.h` pick their access method
        // 1, which reads and writes unaligned words through an `aligned(1)` typedef of the word
        // type, and both headers say in their own comments that this depends on a compiler
        // extension and that method 0, a `memcpy` to and from a local, is the portable one. The
        // trouble with method 1 is not the alignment. It is that a word written as one type is
        // later read as another: the Huffman table is filled eight bytes at a time through
        // `MEM_write64` and decoded two bytes at a time as `U16`, and literals written through
        // `MEM_write16` are copied to the output and hashed through `XXH_read64`. Both are reads
        // the effective type rule forbids, document 09 section 9.1 says the type plane holds the
        // program to that rule, and the replay of tamnd/rucc#1499 reported them on 225 inputs of
        // the OSS-Fuzz corpus. zstd knows what it chose and chose it for speed, so this is the
        // construct C forbids that the project does deliberately, and the same fifteen inputs
        // built with method 0 report nothing of either kind. What is measured here is zstd's own
        // portable code, and the two names stay in this list where anyone counting can see them.
        defines: &["ZSTD_DISABLE_ASM=1", "MEM_FORCE_MEMORY_ACCESS=0", "XXH_FORCE_MEMORY_ACCESS=0"],
        configure: None,
        known: &[],
        pending: Some(
            "the monitor's two reports are not held to a list yet, and they are the same question \
             twice. zstd keeps every position as a 32 bit index and one pointer that turns an \
             index into an address, and ZSTD_window_update computes that pointer as ip - \
             distanceFromBase, which is the caller's buffer moved back by everything the \
             compressor has seen. That derivation leaves the object it came from, which is J2, and \
             the pointer itself is never dereferenced: every match finder reads through it at base \
             + matchIndex, which lands back inside the buffer it came from, and every one of those \
             reads is permitted and every answer is right. What the two are worth is question 12 of \
             spec/safe-memory/17-open-questions.md with a library behind it rather than a test \
             case, and it is tamnd/rucc#1417. There were 56 more reports here until \
             tamnd/rucc#1429, and they were an alignment this compiler got wrong rather than \
             anything zstd does.",
        ),
    },
    Project {
        name: "libwebp",
        variable: "RUCC_LIBWEBP_SOURCE",
        marker: "src/webp/decode.h",
        usual: &["libwebp"],
        sources: &[
            "sharpyuv/sharpyuv.c",
            "sharpyuv/sharpyuv_cpu.c",
            "sharpyuv/sharpyuv_csp.c",
            "sharpyuv/sharpyuv_dsp.c",
            "sharpyuv/sharpyuv_gamma.c",
            "sharpyuv/sharpyuv_neon.c",
            "sharpyuv/sharpyuv_sse2.c",
            "src/dec/alpha_dec.c",
            "src/dec/buffer_dec.c",
            "src/dec/frame_dec.c",
            "src/dec/idec_dec.c",
            "src/dec/io_dec.c",
            "src/dec/quant_dec.c",
            "src/dec/tree_dec.c",
            "src/dec/vp8_dec.c",
            "src/dec/vp8l_dec.c",
            "src/dec/webp_dec.c",
            "src/demux/anim_decode.c",
            "src/demux/demux.c",
            "src/dsp/alpha_processing.c",
            "src/dsp/alpha_processing_mips_dsp_r2.c",
            "src/dsp/alpha_processing_neon.c",
            "src/dsp/alpha_processing_sse2.c",
            "src/dsp/alpha_processing_sse41.c",
            "src/dsp/cost.c",
            "src/dsp/cost_mips32.c",
            "src/dsp/cost_mips_dsp_r2.c",
            "src/dsp/cost_neon.c",
            "src/dsp/cost_sse2.c",
            "src/dsp/cpu.c",
            "src/dsp/dec.c",
            "src/dsp/dec_clip_tables.c",
            "src/dsp/dec_mips32.c",
            "src/dsp/dec_mips_dsp_r2.c",
            "src/dsp/dec_msa.c",
            "src/dsp/dec_neon.c",
            "src/dsp/dec_sse2.c",
            "src/dsp/dec_sse41.c",
            "src/dsp/enc.c",
            "src/dsp/enc_mips32.c",
            "src/dsp/enc_mips_dsp_r2.c",
            "src/dsp/enc_msa.c",
            "src/dsp/enc_neon.c",
            "src/dsp/enc_sse2.c",
            "src/dsp/enc_sse41.c",
            "src/dsp/filters.c",
            "src/dsp/filters_mips_dsp_r2.c",
            "src/dsp/filters_msa.c",
            "src/dsp/filters_neon.c",
            "src/dsp/filters_sse2.c",
            "src/dsp/lossless.c",
            "src/dsp/lossless_avx2.c",
            "src/dsp/lossless_enc.c",
            "src/dsp/lossless_enc_avx2.c",
            "src/dsp/lossless_enc_mips32.c",
            "src/dsp/lossless_enc_mips_dsp_r2.c",
            "src/dsp/lossless_enc_msa.c",
            "src/dsp/lossless_enc_neon.c",
            "src/dsp/lossless_enc_sse2.c",
            "src/dsp/lossless_enc_sse41.c",
            "src/dsp/lossless_mips_dsp_r2.c",
            "src/dsp/lossless_msa.c",
            "src/dsp/lossless_neon.c",
            "src/dsp/lossless_sse2.c",
            "src/dsp/lossless_sse41.c",
            "src/dsp/rescaler.c",
            "src/dsp/rescaler_mips32.c",
            "src/dsp/rescaler_mips_dsp_r2.c",
            "src/dsp/rescaler_msa.c",
            "src/dsp/rescaler_neon.c",
            "src/dsp/rescaler_sse2.c",
            "src/dsp/ssim.c",
            "src/dsp/ssim_sse2.c",
            "src/dsp/upsampling.c",
            "src/dsp/upsampling_mips_dsp_r2.c",
            "src/dsp/upsampling_msa.c",
            "src/dsp/upsampling_neon.c",
            "src/dsp/upsampling_sse2.c",
            "src/dsp/upsampling_sse41.c",
            "src/dsp/yuv.c",
            "src/dsp/yuv_mips32.c",
            "src/dsp/yuv_mips_dsp_r2.c",
            "src/dsp/yuv_neon.c",
            "src/dsp/yuv_sse2.c",
            "src/dsp/yuv_sse41.c",
            "src/enc/alpha_enc.c",
            "src/enc/analysis_enc.c",
            "src/enc/backward_references_cost_enc.c",
            "src/enc/backward_references_enc.c",
            "src/enc/config_enc.c",
            "src/enc/cost_enc.c",
            "src/enc/filter_enc.c",
            "src/enc/frame_enc.c",
            "src/enc/histogram_enc.c",
            "src/enc/iterator_enc.c",
            "src/enc/near_lossless_enc.c",
            "src/enc/picture_csp_enc.c",
            "src/enc/picture_enc.c",
            "src/enc/picture_psnr_enc.c",
            "src/enc/picture_rescale_enc.c",
            "src/enc/picture_tools_enc.c",
            "src/enc/predictor_enc.c",
            "src/enc/quant_enc.c",
            "src/enc/syntax_enc.c",
            "src/enc/token_enc.c",
            "src/enc/tree_enc.c",
            "src/enc/vp8l_enc.c",
            "src/enc/webp_enc.c",
            "src/mux/anim_encode.c",
            "src/mux/muxedit.c",
            "src/mux/muxinternal.c",
            "src/mux/muxread.c",
            "src/utils/bit_reader_utils.c",
            "src/utils/bit_writer_utils.c",
            "src/utils/color_cache_utils.c",
            "src/utils/filters_utils.c",
            "src/utils/huffman_encode_utils.c",
            "src/utils/huffman_utils.c",
            "src/utils/palette.c",
            "src/utils/quant_levels_dec_utils.c",
            "src/utils/quant_levels_utils.c",
            "src/utils/random_utils.c",
            "src/utils/rescaler_utils.c",
            "src/utils/thread_utils.c",
            "src/utils/utils.c",
        ],
        // Where the public headers are, so the workload writes `#include <webp/encode.h>` the way
        // any caller of this library does. The library's own files reach each other from the top of
        // the tree, which the always searched source directory already covers.
        includes: &["src"],
        // Nothing, which is the interesting part. libwebp's configure looks for a threading library
        // and for four image format libraries it can read and write files with, and none of that is
        // wanted here: the workload hands the encoder a buffer it built itself and reads the answer
        // back out of another buffer, so there is no file and no format to decode one from, and
        // without WEBP_USE_THREAD the one file that would have started threads takes the path it
        // takes on a machine with no threads. What is left is the codec, which is what this row is
        // for.
        defines: &[],
        // Thirteen, and two stories. The first eight are the init plane over a lane libwebp leaves
        // alone on purpose and then loads anyway, and the other five are the type plane over a run
        // of pixels the lossless decoder fills eight bytes at a time and reads back four at a time.
        configure: None,
        known: &[
            Known {
                judgement: 1,
                bytes: 8,
                why: "AccumulateRGB in src/enc/picture_csp_enc.c writes three of every four \
                      uint16_t of the row it averages into and leaves the fourth alone, because \
                      the fourth is where the alpha would go and there is no alpha on this path. \
                      What reads the row back is ConvertRGBA32ToUV_SSE2 in src/dsp/yuv_sse2.c, \
                      which takes sixteen bytes at a time and so reads the lane nobody wrote along \
                      with the three it wants. The value is shuffled out again and no answer \
                      depends on it, and the read still happened, which is what the init plane \
                      says. libwebp knows about it: the loop has a WEBP_MSAN arm that zeroes the \
                      lane for this reason and names https://crbug.com/webp/573 beside it. Same \
                      class as zlib's slide_hash above, and the second project to produce it. This \
                      is the low half of the first of the four sixteen byte loads \
                      RGBA32PackedToPlanar_16b_SSE2 makes, the one at rgbx + 0.",
            },
            Known {
                judgement: 1,
                bytes: 8,
                why: "The high half of the load at rgbx + 0. There was a time when all four loads \
                      went through one out of line _mm_loadu_si128 and so arrived at the same two \
                      check sites, and since tamnd/rucc#1842 inlines always_inline at every level \
                      each LOAD_16 is a site of its own with a low and a high half.",
            },
            Known {
                judgement: 1,
                bytes: 8,
                why: "The low half of the load at rgbx + 8 in RGBA32PackedToPlanar_16b_SSE2, \
                      which reads the same unwritten alpha lane of the averaged row.",
            },
            Known {
                judgement: 1,
                bytes: 8,
                why: "The high half of the load at rgbx + 8 in RGBA32PackedToPlanar_16b_SSE2, \
                      which reads the same unwritten alpha lane of the averaged row.",
            },
            Known {
                judgement: 1,
                bytes: 8,
                why: "The low half of the load at rgbx + 16 in RGBA32PackedToPlanar_16b_SSE2, \
                      which reads the same unwritten alpha lane of the averaged row.",
            },
            Known {
                judgement: 1,
                bytes: 8,
                why: "The high half of the load at rgbx + 16 in RGBA32PackedToPlanar_16b_SSE2, \
                      which reads the same unwritten alpha lane of the averaged row.",
            },
            Known {
                judgement: 1,
                bytes: 8,
                why: "The low half of the load at rgbx + 24 in RGBA32PackedToPlanar_16b_SSE2, \
                      which reads the same unwritten alpha lane of the averaged row.",
            },
            Known {
                judgement: 1,
                bytes: 8,
                why: "The high half of the load at rgbx + 24 in RGBA32PackedToPlanar_16b_SSE2, \
                      which reads the same unwritten alpha lane of the averaged row.",
            },
            Known {
                judgement: 1,
                bytes: 4,
                why: "CopySmallPattern32b in src/dec/vp8l_dec.c fills a run of pixels from a back \
                      reference one or two pixels behind it by casting the uint32_t* destination \
                      to uint64_t* and storing the repeated pattern eight bytes at a time. That \
                      leaves eight bytes described as one eight byte object, and every later read \
                      of one of those pixels as a uint32_t asks for a four byte one and is told \
                      no. The cast is the defect rather than the read: after the store the storage \
                      has an effective type of uint64_t and 6.5p7 does not let it be read as \
                      anything else. Every compiler in practice does what libwebp wants here and \
                      every answer this workload checks is right. Five sites read those pixels \
                      back, and this is CopyBlock32b itself, where a later back reference copies \
                      pixel by pixel over a run an earlier pattern copy wrote.",
            },
            Known {
                judgement: 1,
                bytes: 4,
                why: "The same pattern copy read back by ReadHuffmanCodes in src/dec/vp8l_dec.c, \
                      which walks the decoded huffman image a uint32_t at a time to find out how \
                      many trees the picture has.",
            },
            Known {
                judgement: 1,
                bytes: 4,
                why: "The same pattern copy read back by DecodeImageData in src/dec/vp8l_dec.c, \
                      which hands every pixel it has emitted to VP8LColorCacheInsert as a uint32_t \
                      once the run that produced it is finished.",
            },
            Known {
                judgement: 1,
                bytes: 4,
                why: "The same pattern copy read back by ColorSpaceInverseTransform_C in \
                      src/dsp/lossless.c, which undoes the cross colour transform a pixel at a \
                      time over a row the decoder built.",
            },
            Known {
                judgement: 1,
                bytes: 4,
                why: "The same pattern copy read back by PredictorInverseTransform_C in \
                      src/dsp/lossless.c, which is the other inverse transform over the same rows \
                      and reads the row above as well as the row it is writing.",
            },
        ],
        pending: None,
    },
    Project {
        name: "libjpeg-turbo",
        variable: "RUCC_LIBJPEG_TURBO_SOURCE",
        marker: "src/turbojpeg.h",
        usual: &["libjpeg-turbo"],
        // The list cmake builds into turbojpeg-static, in its order: the libjpeg core with each of
        // the files that come in three precisions taken through the wrapper that sets
        // BITS_IN_JSAMPLE and includes it, arithmetic coding, which is on by default, then the
        // TurboJPEG layer and the image file readers and writers it carries, then the PNG reader
        // and the zlib those need, which the tarball bundles under src/spng. The workload loads
        // and saves no files, and the readers are here anyway because turbojpeg.c calls them and a
        // link of every object, which is what this is, needs everything a called function names.
        sources: &[
            "src/jcapimin.c",
            "src/wrapper/jcapistd-8.c",
            "src/wrapper/jcapistd-12.c",
            "src/wrapper/jcapistd-16.c",
            "src/wrapper/jccoefct-8.c",
            "src/wrapper/jccoefct-12.c",
            "src/wrapper/jccolor-8.c",
            "src/wrapper/jccolor-12.c",
            "src/wrapper/jccolor-16.c",
            "src/wrapper/jcdctmgr-8.c",
            "src/wrapper/jcdctmgr-12.c",
            "src/wrapper/jcdiffct-8.c",
            "src/wrapper/jcdiffct-12.c",
            "src/wrapper/jcdiffct-16.c",
            "src/jchuff.c",
            "src/jcicc.c",
            "src/jcinit.c",
            "src/jclhuff.c",
            "src/wrapper/jclossls-8.c",
            "src/wrapper/jclossls-12.c",
            "src/wrapper/jclossls-16.c",
            "src/wrapper/jcmainct-8.c",
            "src/wrapper/jcmainct-12.c",
            "src/wrapper/jcmainct-16.c",
            "src/jcmarker.c",
            "src/jcmaster.c",
            "src/jcomapi.c",
            "src/jcparam.c",
            "src/jcphuff.c",
            "src/wrapper/jcprepct-8.c",
            "src/wrapper/jcprepct-12.c",
            "src/wrapper/jcprepct-16.c",
            "src/wrapper/jcsample-8.c",
            "src/wrapper/jcsample-12.c",
            "src/wrapper/jcsample-16.c",
            "src/jctrans.c",
            "src/jdapimin.c",
            "src/wrapper/jdapistd-8.c",
            "src/wrapper/jdapistd-12.c",
            "src/wrapper/jdapistd-16.c",
            "src/jdatadst.c",
            "src/jdatasrc.c",
            "src/wrapper/jdcoefct-8.c",
            "src/wrapper/jdcoefct-12.c",
            "src/wrapper/jdcolor-8.c",
            "src/wrapper/jdcolor-12.c",
            "src/wrapper/jdcolor-16.c",
            "src/wrapper/jddctmgr-8.c",
            "src/wrapper/jddctmgr-12.c",
            "src/wrapper/jddiffct-8.c",
            "src/wrapper/jddiffct-12.c",
            "src/wrapper/jddiffct-16.c",
            "src/jdhuff.c",
            "src/jdicc.c",
            "src/jdinput.c",
            "src/jdlhuff.c",
            "src/wrapper/jdlossls-8.c",
            "src/wrapper/jdlossls-12.c",
            "src/wrapper/jdlossls-16.c",
            "src/wrapper/jdmainct-8.c",
            "src/wrapper/jdmainct-12.c",
            "src/wrapper/jdmainct-16.c",
            "src/jdmarker.c",
            "src/jdmaster.c",
            "src/wrapper/jdmerge-8.c",
            "src/wrapper/jdmerge-12.c",
            "src/jdphuff.c",
            "src/wrapper/jdpostct-8.c",
            "src/wrapper/jdpostct-12.c",
            "src/wrapper/jdpostct-16.c",
            "src/wrapper/jdsample-8.c",
            "src/wrapper/jdsample-12.c",
            "src/wrapper/jdsample-16.c",
            "src/jdtrans.c",
            "src/jerror.c",
            "src/jfdctflt.c",
            "src/wrapper/jfdctfst-8.c",
            "src/wrapper/jfdctfst-12.c",
            "src/wrapper/jfdctint-8.c",
            "src/wrapper/jfdctint-12.c",
            "src/wrapper/jidctflt-8.c",
            "src/wrapper/jidctflt-12.c",
            "src/wrapper/jidctfst-8.c",
            "src/wrapper/jidctfst-12.c",
            "src/wrapper/jidctint-8.c",
            "src/wrapper/jidctint-12.c",
            "src/wrapper/jidctred-8.c",
            "src/wrapper/jidctred-12.c",
            "src/jmemmgr.c",
            "src/jmemnobs.c",
            "src/jpeg_nbits.c",
            "src/wrapper/jquant1-8.c",
            "src/wrapper/jquant1-12.c",
            "src/wrapper/jquant2-8.c",
            "src/wrapper/jquant2-12.c",
            "src/wrapper/jutils-8.c",
            "src/wrapper/jutils-12.c",
            "src/wrapper/jutils-16.c",
            "src/jaricom.c",
            "src/jcarith.c",
            "src/jdarith.c",
            "src/turbojpeg.c",
            "src/transupp.c",
            "src/jdatadst-tj.c",
            "src/jdatasrc-tj.c",
            "src/rdbmp.c",
            "src/wrapper/rdpng-8.c",
            "src/wrapper/rdpng-12.c",
            "src/wrapper/rdpng-16.c",
            "src/wrapper/rdppm-8.c",
            "src/wrapper/rdppm-12.c",
            "src/wrapper/rdppm-16.c",
            "src/wrbmp.c",
            "src/wrapper/wrpng-8.c",
            "src/wrapper/wrpng-12.c",
            "src/wrapper/wrpng-16.c",
            "src/wrapper/wrppm-8.c",
            "src/wrapper/wrppm-12.c",
            "src/wrapper/wrppm-16.c",
            "src/spng/spng.c",
            "src/spng/zlib/adler32.c",
            "src/spng/zlib/compress.c",
            "src/spng/zlib/crc32.c",
            "src/spng/zlib/deflate.c",
            "src/spng/zlib/inffast.c",
            "src/spng/zlib/inflate.c",
            "src/spng/zlib/inftrees.c",
            "src/spng/zlib/trees.c",
            "src/spng/zlib/zutil.c",
        ],
        // The public headers, and the two the bundled PNG reader wants, so that spng.c finds the
        // zlib that came with it rather than whichever one the machine has.
        includes: &["src", "src/spng", "src/spng/zlib"],
        // What CMakeLists.txt puts on turbojpeg-static as a whole. The first three turn on the
        // file formats the TurboJPEG layer can load and save, which have to be on for turbojpeg.c
        // to compile against the readers above, and SPNG_STATIC says libspng is linked in rather
        // than loaded.
        defines: &["BMP_SUPPORTED", "PNG_SUPPORTED", "PPM_SUPPORTED", "SPNG_STATIC"],
        // What cmake writes into jconfig.h, jconfigint.h and jversion.h on an x86-64 Linux machine
        // with WITH_SIMD off, which is the one setting chosen rather than found. The SIMD code is
        // assembly, and assembly is outside what the monitor can see, so a build with it on would
        // send every inner loop of the codec somewhere no check can follow. The version is the
        // version of the tarball this row was written against and is only ever printed.
        configure: Some(Configure {
            templates: &["src/jconfig.h.in", "src/jconfigint.h.in", "src/jversion.h.in"],
            settings: &[
                ("JPEG_LIB_VERSION", "62"),
                ("VERSION", "3.2.0"),
                ("LIBJPEG_TURBO_VERSION_NUMBER", "3002000"),
                ("C_ARITH_CODING_SUPPORTED", "1"),
                ("D_ARITH_CODING_SUPPORTED", "1"),
                ("BUILD", "rucc"),
                ("HIDDEN", "__attribute__((visibility(\"hidden\")))"),
                ("INLINE", "__inline__ __attribute__((always_inline))"),
                ("THREAD_LOCAL", "__thread"),
                ("CMAKE_PROJECT_NAME", "libjpeg-turbo"),
                ("SIZE_T", "8"),
                ("HAVE_BUILTIN_CTZL", "1"),
                ("SIMD_ARCHITECTURE", "NONE"),
                ("COPYRIGHT_YEAR", "1991-2026"),
            ],
        }),
        known: &[],
        pending: None,
    },
];

/// Builds each library at each level, runs its workload, and holds it to its answers and to what
/// the monitor said about it.
///
/// # Errors
///
/// [`Error::Failed`] when a build did not link, a run did not exit cleanly, the answers came back
/// wrong, or the monitor said something other than what the row says it should, with one line per
/// thing that went wrong. [`Error::Io`] when the check could not be run at all, which is the
/// compiler not building or no way to run an x86-64 Linux program.
pub(crate) fn libraries() -> Result<()> {
    let runner = Runner::find("this check")?;
    let mut problems = Vec::new();
    let mut ran_any = false;

    for project in PROJECTS {
        let Some(source) = found(project) else {
            // Not a failure, the same way a mac with no glibc is not one. What is worth printing is
            // where to get the sources, because a person reading this line is a person about to go
            // and look for them.
            println!(
                "{}: no sources on this machine, so nothing was built. Unpack the project's own \
                 tarball and point {} at it.",
                project.name, project.variable
            );
            continue;
        };
        ran_any = true;
        println!(
            "{}: {}, -fsafety=detect at -O0 and -O2, {runner}",
            project.name,
            source.display()
        );
        if let Some(waiting) = project.pending {
            println!("{}: {waiting}", project.name);
        }
        let work = build(project, &source)?;
        let ran = safety::read(&runner.run(&work, "the workload")?);
        judge(project, &ran, &mut problems);
    }

    if !ran_any {
        return Ok(());
    }
    if problems.is_empty() {
        println!("libraries: every level links, runs and answers correctly");
        return Ok(());
    }
    Err(Error::Failed { task: "libraries", problems })
}

/// Reads one project's two runs and says what was wrong with them.
fn judge(
    project: &Project,
    ran: &std::collections::BTreeMap<String, safety::Ran>,
    out: &mut Vec<String>,
) {
    for level in LEVELS {
        let name = project.name;
        let Some(ran) = ran.get(level) else {
            out.push(format!("{name} {level}: did not run"));
            continue;
        };
        match ran.status {
            None => out
                .push(format!("{name} {level}: did not link.\n{}", indent(ran.output.trim_end()))),
            Some(0) if ran.output.contains(CORRECT) => {}
            _ => out.push(format!(
                "{name} {level}: ran and got the wrong answers, or did not finish.\n{}",
                indent(ran.output.trim_end())
            )),
        }
        said(project, level, &ran.output, out);
    }
}

/// Holds what the monitor said against what the row says it should have said.
///
/// Both directions. A report nobody listed is a false positive, since the workload does nothing
/// wrong. A listed report that did not arrive means the row is describing a library that no longer
/// does what it says, or a check that stopped looking, and either one is worth a person's time.
fn said(project: &Project, level: &str, output: &str, out: &mut Vec<String>) {
    if project.pending.is_some() {
        return;
    }
    let mut wanted: Vec<&Known> = project.known.iter().collect();
    for (judgement, bytes) in reports(output) {
        match wanted.iter().position(|k| k.judgement == judgement && k.bytes == bytes) {
            Some(at) => {
                wanted.remove(at);
            }
            None => out.push(format!(
                "{} {level}: the monitor made a report nothing here expects, J{judgement} over \
                 {bytes} bytes, so this is a false positive or a finding somebody should write \
                 down.\n{}",
                project.name,
                indent(output.trim_end())
            )),
        }
    }
    for missing in wanted {
        out.push(format!(
            "{} {level}: the monitor did not report J{} over {} bytes, which this row says it \
             should. {}",
            project.name, missing.judgement, missing.bytes, missing.why
        ));
    }
}

/// Every report in a run, as the judgement number and the width of the access.
///
/// The runs are made in the deduplicating posture, so one site says its piece once however many
/// times it is reached, and what comes back out of here is one entry per site rather than one per
/// occurrence.
fn reports(output: &str) -> Vec<(u32, u32)> {
    let mut found = Vec::new();
    for chunk in output.split(BANNER).skip(1) {
        let Some(judgement) = number_after(chunk, "  judgement J") else {
            continue;
        };
        let Some(bytes) = number_before(chunk, " bytes at ") else {
            continue;
        };
        found.push((judgement, bytes));
    }
    found
}

/// The number that starts right after `tag`.
pub(crate) fn number_after(text: &str, tag: &str) -> Option<u32> {
    let at = text.find(tag)? + tag.len();
    let digits: String = text[at..].chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// The number that ends right before `tag`.
pub(crate) fn number_before(text: &str, tag: &str) -> Option<u32> {
    let at = text.find(tag)?;
    let digits: String =
        text[..at].chars().rev().take_while(char::is_ascii_digit).collect::<String>();
    digits.chars().rev().collect::<String>().parse().ok()
}

/// Where a project's sources are, if they are anywhere this knows to look.
pub(crate) fn found(project: &Project) -> Option<PathBuf> {
    if let Some(said) = std::env::var_os(project.variable) {
        if let Some(said) = declared(Path::new(&said), project.marker) {
            return Some(said);
        }
    }
    usual(project)
}

/// What a path somebody set is worth, which is nothing when the sources are not under it.
///
/// Either the directory or the marker file inside it, because a variable that used to name one file
/// is easy to leave naming that file. A variable pointing at neither reads as the variable not
/// being set, since the sentence that prints in that case names the variable and is the right thing
/// to read either way.
fn declared(said: &Path, marker: &str) -> Option<PathBuf> {
    if let Some(holding) = holds(said, marker) {
        return Some(holding);
    }
    if said.is_file() && said.file_name()? == marker {
        return Some(said.parent()?.to_path_buf());
    }
    None
}

/// The directory under `dir` that really holds the sources, which is `dir` itself or the `src` under
/// it.
///
/// Both, because a project is as likely to unpack its C into a `src` directory as to leave it at the
/// top and the person setting the variable should not have to remember which this one does. Only one
/// level, for the reason [`usual`] gives about hunting.
fn holds(dir: &Path, marker: &str) -> Option<PathBuf> {
    if !dir.is_dir() {
        return None;
    }
    if dir.join(marker).is_file() {
        return Some(dir.to_path_buf());
    }
    let inside = dir.join("src");
    inside.join(marker).is_file().then_some(inside)
}

/// Where the sources are when nobody said, which is the directory the project's tarball unpacks to
/// under one of the two places a person is likely to have put it.
///
/// Neither place is searched recursively, because a check that goes hunting through a home
/// directory for somebody else's C is a check that will one day find the wrong copy of it.
fn usual(project: &Project) -> Option<PathBuf> {
    let homes =
        [std::env::var_os("HOME").map(PathBuf::from), Some(PathBuf::from("/usr/local/src"))];
    for home in homes.into_iter().flatten() {
        for stem in project.usual {
            let Ok(entries) = std::fs::read_dir(&home) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(name) = name.to_str() else {
                    continue;
                };
                if !name.starts_with(stem) {
                    continue;
                }
                if let Some(holding) = holds(&entry.path(), project.marker) {
                    return Some(holding);
                }
            }
        }
    }
    None
}

/// Compiles a project and its workload at each level and leaves a directory the runner can take.
///
/// Assembly rather than objects, so that the link is the runner's and a machine that cannot run an
/// x86-64 program can still do everything up to it. That is the same arrangement the safety suite
/// has, and it is what lets the container do one link and one run rather than being handed a binary
/// built somewhere it cannot read.
fn build(project: &Project, source: &Path) -> Result<PathBuf> {
    let work = target_dir().join("libraries").join(project.name);
    if work.exists() {
        std::fs::remove_dir_all(&work)
            .map_err(|e| Error::Io(format!("could not clear {}: {e}", work.display())))?;
    }
    std::fs::create_dir_all(&work)
        .map_err(|e| Error::Io(format!("could not make {}: {e}", work.display())))?;

    let configured = configure(project, source, &work)?;
    let rucc = cost::compiler()?;
    let archive = crate::staticlib("rucc-safe-rt", TRIPLE)?;
    std::fs::copy(&archive, work.join("safe-rt.a"))
        .map_err(|e| Error::Io(format!("could not copy {}: {e}", archive.display())))?;

    let driver = root().join("tests").join(project.name).join("a-real-workload.c");
    let mut problems = Vec::new();
    for level in LEVELS {
        for (file, stem) in files(project, source, &driver) {
            let mut command = Command::new(&rucc);
            command
                .args([
                    "-S",
                    &format!("--target={TRIPLE}"),
                    "-fsafety=detect",
                    level,
                    crate::VERIFY,
                ])
                .arg("-I")
                .arg(source)
                .args(configured.iter().flat_map(|dir| [PathBuf::from("-I"), dir.clone()]))
                .args(
                    project
                        .includes
                        .iter()
                        .flat_map(|inside| [Path::new("-I").to_path_buf(), source.join(inside)]),
                )
                .args(project.defines.iter().map(|define| format!("-D{define}")))
                .arg("-o")
                .arg(work.join(format!("{stem}{level}.s")))
                .arg(&file);
            let out = command
                .current_dir(root())
                .output()
                .map_err(|e| Error::Io(format!("could not run the compiler: {e}")))?;
            if !out.status.success() {
                problems.push(format!(
                    "{} {level}: {} did not compile\n{}",
                    project.name,
                    file.display(),
                    indent(String::from_utf8_lossy(&out.stderr).trim_end())
                ));
            }
        }
    }
    if !problems.is_empty() {
        return Err(Error::Failed { task: "libraries", problems });
    }

    std::fs::write(work.join("run.sh"), script(project))
        .map_err(|e| Error::Io(format!("could not write the script: {e}")))?;
    Ok(work)
}

/// Every file to compile and the name its assembly goes under.
///
/// The workload is called `driver` rather than what it is called in the tree, so that the script
/// below can name it without knowing which project it is running.
///
/// A project that keeps its C in subdirectories gets one flat directory of assembly out, with the
/// path written into the name, because two files called `state.c` in two directories are two files
/// and a name that dropped the directory would quietly be one.
fn files(project: &Project, source: &Path, driver: &Path) -> Vec<(PathBuf, String)> {
    let mut all: Vec<(PathBuf, String)> = project
        .sources
        .iter()
        .map(|name| (source.join(name), name.trim_end_matches(".c").replace('/', "-")))
        .collect();
    all.push((driver.to_path_buf(), "driver".to_owned()));
    all
}

/// The script that links each level and runs it.
///
/// `gcc` rather than rucc's own driver for the link, because what is being checked is the code rucc
/// generated and not the link line it writes. `-no-pie` for the same reason the safety suite uses
/// it: the runtime's planes are addressed absolutely.
///
/// The runs are made in the deduplicating posture rather than the aborting one, because what this
/// check wants is every distinct site the monitor has something to say about and not the first one,
/// and because a library with a report the row expects still has to finish its workload and get the
/// answers right.
///
/// The case names are the levels, so that what comes back reads the way the safety suite's does and
/// can be split by the same reader.
fn script(project: &Project) -> String {
    let stems: Vec<String> = files(project, Path::new(""), Path::new(""))
        .into_iter()
        .map(|(_, stem)| format!("\"{stem}$level.s\""))
        .collect();
    let objects = stems.join(" ");
    let libraries = LIBRARIES.join(" ");
    format!(
        "\
#!/bin/sh
exec 2>/dev/null
here=$(pwd)
out=/tmp/rucc-${{here##*/}}
mkdir -p \"$out\"
RUCC_SAFETY_ON_ERROR=continue
export RUCC_SAFETY_ON_ERROR
for level in -O0 -O2; do
    printf '<<<case %s>>>\\n' \"$level\"
    if gcc -no-pie {objects} safe-rt.a {libraries} \\
        -o \"$out/run$level\" >\"$out/$level.log\" 2>&1; then
        (cd \"$out\" && \"$out/run$level\") >\"$out/$level.out\" 2>&1
        status=$?
        cat \"$out/$level.out\"
        printf '<<<status %s>>>\\n' \"$status\"
    else
        cat \"$out/$level.log\"
        printf '<<<status nolink>>>\\n'
    fi
done
"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The script and the compile loop have to agree about what the files are called, and they are
    /// written out in two places because one is shell and the other is Rust.
    #[test]
    fn the_script_links_the_files_the_build_writes() {
        for project in PROJECTS {
            let text = script(project);
            for level in LEVELS {
                assert!(text.contains(level), "{text}");
            }
            for (_, stem) in files(project, Path::new(""), Path::new("")) {
                assert!(text.contains(&format!("\"{stem}$level.s\"")), "{text}");
            }
        }
    }

    /// Four runs share `/tmp` under the gate and a fixed name would be four of them in one
    /// directory, which is the hazard the safety suite's own test is about.
    #[test]
    fn the_programs_go_somewhere_named_after_the_run() {
        for project in PROJECTS {
            assert!(script(project).contains("out=/tmp/rucc-${here##*/}"), "{}", project.name);
        }
    }

    /// The row is a claim that the monitor will say something, which is only worth making in the
    /// posture that lets the program reach every site rather than stopping at the first.
    #[test]
    fn the_runs_are_made_in_the_posture_that_sees_every_site() {
        for project in PROJECTS {
            assert!(script(project).contains("RUCC_SAFETY_ON_ERROR=continue"), "{}", project.name);
        }
    }

    /// The sentence a workload prints when its arithmetic came out is the thing this check reads,
    /// so it has to be the sentence the workload actually prints, in every project.
    #[test]
    fn every_workload_prints_the_words_this_looks_for() {
        for project in PROJECTS {
            let path = root().join("tests").join(project.name).join("a-real-workload.c");
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|_| panic!("{} is in the tree", path.display()));
            assert!(source.contains(CORRECT), "{CORRECT} is not what {} prints", project.name);
        }
    }

    /// A row that expects a report has to say which site and why, because the judgement and the
    /// width on their own do not tell a later reader anything they could check.
    #[test]
    fn every_expected_report_says_what_it_is() {
        for project in PROJECTS {
            for known in project.known {
                assert!(
                    known.why.len() > 80,
                    "{} says too little about J{}",
                    project.name,
                    known.judgement
                );
            }
        }
    }

    /// Both directions of the expectation, since the whole value of the row is that it fails when
    /// the monitor says more than it should and when it says less.
    #[test]
    fn a_report_is_held_against_the_row_in_both_directions() {
        let quiet = Project {
            name: "quiet",
            variable: "",
            marker: "",
            usual: &[],
            sources: &[],
            includes: &[],
            defines: &[],
            configure: None,
            known: &[],
            pending: None,
        };
        let expecting = Project {
            known: &[Known { judgement: 1, bytes: 2, why: "because the test says so" }],
            ..quiet
        };
        let one = format!("{BANNER}\n  judgement J1, whatever\n  2 bytes at 0x1\n");

        let mut out = Vec::new();
        said(&quiet, "-O0", &one, &mut out);
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].contains("a report nothing here expects"), "{out:?}");

        let mut out = Vec::new();
        said(&expecting, "-O0", &one, &mut out);
        assert!(out.is_empty(), "{out:?}");

        let mut out = Vec::new();
        said(&expecting, "-O0", "a run that said nothing", &mut out);
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].contains("did not report J1 over 2 bytes"), "{out:?}");

        // Two sites that look alike are two reports and the row only excuses one of them, which is
        // the coarseness of matching on the judgement and the width made visible.
        let mut out = Vec::new();
        said(&expecting, "-O0", &format!("{one}{one}"), &mut out);
        assert_eq!(out.len(), 1, "{out:?}");

        // A row waiting on a question is quiet in both directions, since a list it cannot write
        // yet is a list it cannot be held to either way.
        let waiting = Project { pending: Some("waiting on something"), ..expecting };
        let mut out = Vec::new();
        said(&waiting, "-O0", &one, &mut out);
        said(&waiting, "-O0", "a run that said nothing", &mut out);
        assert!(out.is_empty(), "{out:?}");
    }

    /// Only a row that names a question is allowed to be quiet about what the monitor said, and the
    /// reason has to be long enough to be a reason rather than a shrug.
    #[test]
    fn a_row_that_is_waiting_says_what_it_is_waiting_on() {
        for project in PROJECTS {
            let Some(waiting) = project.pending else {
                continue;
            };
            assert!(
                waiting.len() > 200,
                "{}'s pending is too short to say anything: {waiting}",
                project.name
            );
            assert!(
                waiting.contains("tamnd/rucc#"),
                "{} is waiting on something nobody can go and read",
                project.name
            );
        }
    }

    /// An unset variable and a variable pointing at nothing are the same answer, because the
    /// sentence that prints names the variable either way.
    #[test]
    fn a_path_that_is_not_there_reads_as_nothing_being_there() {
        assert_eq!(declared(Path::new("/nowhere/at/all"), "sqlite3.c"), None);
        // A directory with nothing of the project in it is not the project.
        assert_eq!(declared(&root(), "sqlite3.c"), None);
        // The directory and the file inside it are both accepted, because a variable that used to
        // name the file is easy to leave naming the file.
        let tests = root().join("tests").join("zlib");
        assert_eq!(declared(&tests, "a-real-workload.c"), Some(tests.clone()));
        assert_eq!(declared(&tests.join("a-real-workload.c"), "a-real-workload.c"), Some(tests));
    }

    /// The assembly all goes in one directory, so two files with the same name in two of a
    /// project's own directories have to come out with two names.
    #[test]
    fn a_file_in_a_subdirectory_keeps_the_subdirectory_in_its_name() {
        for project in PROJECTS {
            let mut stems: Vec<String> =
                files(project, Path::new(""), Path::new("")).into_iter().map(|(_, s)| s).collect();
            let all = stems.len();
            stems.sort();
            stems.dedup();
            assert_eq!(stems.len(), all, "{} has two files under one name", project.name);
            for stem in &stems {
                assert!(!stem.contains('/'), "{stem} would want a directory that is not made");
            }
        }
    }

    /// Half the projects worth running unpack their C into a `src` directory and half leave it at
    /// the top, and the person setting the variable should not have to know which this one is.
    /// cmake's two rules, a `#cmakedefine` for a value the row gives and for one it does not, and
    /// the `01` form both ways, which is all three of libjpeg-turbo's templates ask for.
    #[test]
    fn a_template_is_filled_in_the_way_cmake_fills_it_in() {
        let settings = [("VERSION", "3.2.0"), ("ARITH", "1"), ("HIDDEN", "__attribute__((x))")];
        let text = "#define VERSION \"@VERSION@\"\n\
                    #cmakedefine ARITH 1\n\
                    #  cmakedefine WITH_SIMD 1\n\
                    #cmakedefine01 ARITH\n\
                    #cmakedefine01 WITH_SIMD\n\
                    #define HIDDEN @HIDDEN@\n\
                    /* mail someone@example.org about it */\n";
        assert_eq!(
            fill(text, &settings).unwrap(),
            "#define VERSION \"3.2.0\"\n\
             #define ARITH 1\n\
             /* #undef WITH_SIMD */\n\
             #define ARITH 1\n\
             #define WITH_SIMD 0\n\
             #define HIDDEN __attribute__((x))\n\
             /* mail someone@example.org about it */\n"
        );
    }

    /// A value the template asks for and the row does not give is an error with the name in it,
    /// rather than the empty string cmake would write.
    #[test]
    fn a_template_asking_for_a_value_the_row_lacks_is_an_error() {
        assert_eq!(fill("#define BUILD \"@BUILD@\"\n", &[]), Err("BUILD".to_owned()));
    }

    #[test]
    fn the_sources_are_taken_from_a_src_directory_as_well_as_from_the_top() {
        let xtask = root().join("xtask");
        assert_eq!(holds(&xtask, "libraries.rs"), Some(xtask.join("src")));
        assert_eq!(holds(&xtask, "Cargo.toml"), Some(xtask.clone()));
        assert_eq!(holds(&xtask, "nothing-of-the-sort.c"), None);
        assert_eq!(declared(&xtask, "libraries.rs"), Some(xtask.join("src")));
    }

    /// The reader has to come back with one entry per site and with the numbers the report wrote,
    /// because everything this check says about false positives rests on it.
    #[test]
    fn the_reader_finds_the_judgement_and_the_width() {
        let text = format!(
            "some output\n{BANNER}\n  judgement J1, an access the capability, the planes or the \
             alignment did not permit\n  2 bytes at 0x00007f655b86485e\n  in instance 3, which is \
             live\nmore output\n{BANNER}\n  judgement J10, whatever that one says\n  16 bytes at \
             0x0000000000001000\n"
        );
        assert_eq!(reports(&text), vec![(1, 2), (10, 16)]);
        assert_eq!(reports("a run that said nothing"), Vec::new());
    }
}
