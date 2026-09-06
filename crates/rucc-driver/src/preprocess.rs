//! Running phase 4 and writing what came out, which is what `-E` asks for.
//!
//! Design: `spec/04-driver-and-cli.md` sections 4.3 and 4.4, and `spec/05-preprocessor.md`.
//!
//! This is the first phase the driver actually runs, so it is also where the file system
//! implementation lives. Everything below the driver reads through the [`FileSystem`] trait,
//! and [`OsFileSystem`] is the one implementation of it that talks to the disk. Keeping it
//! here rather than in `rucc-session` is what keeps the layer rule true: a preprocessor test
//! is a map from path to bytes and cannot accidentally read the machine it runs on.

use std::io;
use std::path::{Path, PathBuf};

use rucc_diag::{Diagnostic, Severity, SourceBytes, SourceMap};
use rucc_pp::{Context, Predef, Preprocessor, PrintOptions};
use rucc_session::{FileSystem, Options, Session};

/// The file system the compiler reads through when it is a compiler rather than a library.
#[derive(Debug, Clone, Copy, Default)]
pub struct OsFileSystem;

impl OsFileSystem {
    /// The one value of this type.
    #[must_use]
    pub fn new() -> OsFileSystem {
        OsFileSystem
    }
}

impl FileSystem for OsFileSystem {
    fn read(&self, path: &Path) -> io::Result<SourceBytes> {
        // Bytes rather than a string. A source file that is not valid UTF-8 is a file this
        // compiler still has to have an opinion about, and phase 1 is where that opinion
        // belongs, not here. Whether the bytes are a mapping or a buffer is `map`'s decision
        // and is invisible from here.
        crate::map::read(path)
    }

    fn identity(&self, path: &Path) -> PathBuf {
        // `canonicalize` is the portable spelling of what GCC does with the device and inode
        // pair: it resolves the relative part, the `..` and the symlinks, so that every route
        // to one header gives one answer. It fails only if the file is not there, and this is
        // asked about files that have just been read, so the fallback is for a file that was
        // deleted between the two calls and it does not matter what it says.
        std::fs::canonicalize(path).unwrap_or_else(|_| rucc_session::path_key(path))
    }
}

/// What preprocessing one file produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preprocessed {
    /// The text to write, empty when the file could not be read.
    pub text: String,
    /// The diagnostics, already rendered, one per line, in the order they were reported.
    pub messages: Vec<String>,
    /// How many of them were errors.
    pub errors: u32,
}

impl Preprocessed {
    /// Whether anything went wrong badly enough that the output should not be used.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.errors > 0
    }
}

/// Preprocesses one file and renders the result.
///
/// `name` is the path as the user wrote it, which is the name the output and every diagnostic
/// about the file use. It is not canonicalised, because a message naming a path nobody typed
/// is a message that is harder to act on.
#[must_use]
pub fn preprocess(opts: &Options, name: &str, fs: &dyn FileSystem) -> Preprocessed {
    let mut sess = Session::new(opts.clone());
    let bytes = match fs.read(Path::new(name)) {
        Ok(bytes) => bytes,
        Err(e) => return failure(format!("{name}: {e}")),
    };
    let Ok(file) = sess.sources.add_shared(name, bytes, None) else {
        return failure(format!("{name}: the source map has no room left for this file"));
    };

    let mut pp = Preprocessor::new();
    let predef = Predef::for_options(opts);
    let mut cx = Context::new(&mut sess.interner, &mut sess.sources, fs, &opts.search);
    cx.lex = rucc_lex::Options::for_dialect(opts.std, opts.gnu_extensions);
    if pp.predefine(&sess.target, &predef, &mut cx).is_err() {
        return failure(format!("{name}: the source map has no room left for the built in macros"));
    }
    let tokens = pp.run(file, &mut cx);
    // `-dM` replaces the output rather than adding to it. The run still happens, and it has
    // to: the table at the end is the one the file left behind, so a `#define` inside an
    // `#ifdef` that was false is correctly absent.
    let text = if opts.dumps.macros {
        rucc_pp::dump_macros(pp.macros(), &sess.interner)
    } else {
        rucc_pp::print(
            file,
            &tokens,
            pp.line_directives(),
            &sess.sources,
            &sess.interner,
            PrintOptions { line_markers: opts.line_markers },
        )
    };

    let mut messages = Vec::new();
    let mut errors = 0;
    for diag in pp.take_diagnostics() {
        // `-w`, for the reason it is read here in the compiler proper.
        if !opts.warnings && diag.severity == Severity::Warning {
            continue;
        }
        let fatal = diag.severity.is_fatal()
            || (diag.severity == Severity::Warning && opts.warnings_are_errors);
        if fatal {
            errors += 1;
        }
        messages.push(render(&diag, &sess.sources, opts.warnings_are_errors));
    }
    Preprocessed { text, messages, errors }
}

/// A result that is nothing but one message, for the failures that happen before there is
/// anything to preprocess.
fn failure(message: String) -> Preprocessed {
    Preprocessed {
        text: String::new(),
        messages: vec![format!("rucc: error: {message}")],
        errors: 1,
    }
}

/// One diagnostic as the lines it prints.
///
/// GCC's shape: the position, the severity, the message, then the code, then any notes
/// underneath. The chain of includes that reached the file comes first, because a diagnostic
/// in a header three levels down is unactionable without the path that got there.
pub(crate) fn render(diag: &Diagnostic, sources: &SourceMap, warnings_are_errors: bool) -> String {
    let mut out = String::new();
    let mut chain = sources.include_stack(diag.span.lo);
    chain.reverse();
    for (at, from) in chain.iter().enumerate() {
        let lead = if at == 0 { "In file included from" } else { "                 from" };
        out.push_str(&format!("{lead} {}:\n", sources.render_position(from.lo)));
    }
    out.push_str(&line(diag, sources, warnings_are_errors));
    for child in &diag.children {
        out.push('\n');
        out.push_str(&line(child, sources, false));
    }
    out
}

/// The one line a diagnostic or one of its notes prints.
fn line(diag: &Diagnostic, sources: &SourceMap, warnings_are_errors: bool) -> String {
    let severity = if diag.severity == Severity::Warning && warnings_are_errors {
        // Not a relabelling for its own sake. A build that turned warnings into errors and
        // then reads "warning" next to a failed compilation has to go looking for why it
        // failed, and the answer is right here.
        "error"
    } else {
        diag.severity.as_str()
    };
    let position = if diag.span.is_dummy() {
        "rucc".to_owned()
    } else {
        sources.render_position(diag.span.lo)
    };
    match diag.code {
        Some(code) => format!("{position}: {severity}: {} [{code}]", diag.message),
        None => format!("{position}: {severity}: {}", diag.message),
    }
}

#[cfg(test)]
mod tests {
    use rucc_session::MemoryFileSystem;
    use rucc_target::Triple;

    use super::*;

    fn options() -> Options {
        Options::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap())
    }

    /// The path an include search produces for `name` under `dir`.
    ///
    /// The search joins the two with the platform's separator, so an expectation with a slash
    /// written into it is an expectation about Unix rather than about the preprocessor, and it
    /// fails on Windows for a reason that has nothing to do with what the test is checking.
    fn at(dir: &str, name: &str) -> String {
        Path::new(dir).join(name).display().to_string()
    }

    fn run(opts: &Options, files: &[(&str, &str)]) -> Preprocessed {
        let mut fs = MemoryFileSystem::new();
        for (path, text) in files {
            fs.insert(*path, (*text).to_owned().into_bytes());
        }
        preprocess(opts, files[0].0, &fs)
    }

    #[test]
    fn a_file_that_is_not_there_says_so_and_produces_nothing() {
        let fs = MemoryFileSystem::new();
        let result = preprocess(&options(), "/nope.c", &fs);
        assert!(result.failed());
        assert!(result.messages[0].contains("/nope.c"), "{:?}", result.messages);
        assert!(result.text.is_empty());
    }

    #[test]
    fn the_output_is_the_expanded_text_with_a_line_marker_on_top() {
        let result = run(&options(), &[("/main.c", "#define N 2\nint a[N];\n")]);
        assert_eq!(result.messages, Vec::<String>::new());
        assert_eq!(result.text, "# 1 \"/main.c\"\n\nint a[2];\n");
    }

    #[test]
    fn the_predefined_macros_are_there_without_being_asked_for() {
        let result = run(&options(), &[("/main.c", "__SIZEOF_LONG__ __x86_64__\n")]);
        assert_eq!(result.text, "# 1 \"/main.c\"\n8 1\n");
    }

    #[test]
    fn dash_d_and_dash_u_reach_the_macro_table() {
        let mut opts = options();
        opts.defines.push("FOO=41+1".to_owned());
        opts.defines.push("BAR".to_owned());
        opts.undefines.push("__x86_64__".to_owned());
        let result = run(&opts, &[("/main.c", "FOO BAR\n#ifdef __x86_64__\ngone\n#endif\n")]);
        // `41+1` with no space in it, which is what it was written as on the command line and
        // what GCC prints. The three tokens all came out of the one expansion of `FOO`, and a
        // paste is only worth avoiding where a macro put two tokens together that the person
        // did not write together.
        assert_eq!(result.text, "# 1 \"/main.c\"\n41+1 1\n");
    }

    #[test]
    fn dash_i_is_where_an_angled_include_looks() {
        let mut opts = options();
        opts.search.push_bracket("/inc");
        let files = [("/main.c", "#include <one.h>\nint after;\n"), ("/inc/one.h", "int in_it;\n")];
        let result = run(&opts, &files);
        // A line marker's file name is a string literal, so a separator that is a backslash
        // comes out escaped, which is what GCC does and what a reader of the output has to be
        // able to parse back.
        let one = at("/inc", "one.h").replace('\\', "\\\\");
        let expected = format!(
            "# 1 \"/main.c\"\n# 1 \"{one}\" 1\nint in_it;\n# 2 \"/main.c\" 2\nint after;\n"
        );
        assert_eq!(result.text, expected);
    }

    #[test]
    fn dash_p_leaves_the_markers_out() {
        let mut opts = options();
        opts.line_markers = false;
        opts.search.push_bracket("/inc");
        let files = [("/main.c", "#include <one.h>\nint after;\n"), ("/inc/one.h", "int in_it;\n")];
        assert_eq!(run(&opts, &files).text, "int in_it;\nint after;\n");
    }

    #[test]
    fn a_diagnostic_says_where_it_is_and_carries_its_code() {
        let result = run(&options(), &[("/main.c", "#error no\n")]);
        assert_eq!(result.errors, 1);
        assert!(
            result.messages[0].starts_with("/main.c:1:8: error: no ["),
            "{:?}",
            result.messages
        );
    }

    #[test]
    fn a_diagnostic_in_a_header_prints_the_chain_that_reached_it() {
        let mut opts = options();
        opts.search.push_bracket("/inc");
        let files = [
            ("/main.c", "#include <one.h>\n"),
            ("/inc/one.h", "#include <two.h>\n"),
            ("/inc/two.h", "#error deep\n"),
        ];
        let result = run(&opts, &files);
        let text = result.messages.join("\n");
        assert!(text.starts_with("In file included from /main.c:1:1:\n"), "{text}");
        assert!(
            text.contains(&format!("                 from {}:1:1:\n", at("/inc", "one.h"))),
            "{text}"
        );
        assert!(text.contains(&format!("{}:1:8: error: deep", at("/inc", "two.h"))), "{text}");
    }

    #[test]
    fn werror_turns_a_warning_into_an_error_in_the_count_and_in_the_word() {
        let source = "#warning careful\n";
        let plain = run(&options(), &[("/main.c", source)]);
        assert_eq!(plain.errors, 0);
        assert!(plain.messages[0].contains("warning: careful"), "{:?}", plain.messages);

        let mut opts = options();
        opts.warnings_are_errors = true;
        let strict = run(&opts, &[("/main.c", source)]);
        assert_eq!(strict.errors, 1);
        assert!(strict.messages[0].contains("error: careful"), "{:?}", strict.messages);
    }

    #[test]
    fn dash_dm_prints_the_macros_the_file_left_behind_and_not_the_file() {
        let source = "#define KEPT 1\n#define GONE 2\n#undef GONE\n#ifdef NEVER\n#define \
                      HIDDEN 3\n#endif\nint x;\n";
        let result = run(&options(), &[("/main.c", source)]);
        assert_eq!(result.errors, 0);
        assert!(result.text.contains("int x;"), "the output is the file without -dM");
        assert!(!result.text.contains("#define"), "a directive line is not part of the output");

        let mut opts = options();
        opts.dumps.macros = true;
        let dumped = run(&opts, &[("/main.c", source)]);
        assert!(dumped.text.contains("#define KEPT 1\n"), "{}", dumped.text);
        assert!(!dumped.text.contains("int x;"), "-dM replaces the output rather than adding");
        // Undefined is gone, and a define the conditional skipped was never made. The dump is
        // the table at the end of the run and not a list of the lines that were written.
        assert!(!dumped.text.contains("GONE"), "{}", dumped.text);
        assert!(!dumped.text.contains("HIDDEN"), "{}", dumped.text);
        // The predefined set is in there too, because it is defined the same way everything
        // else is, which is the whole reason this output can be diffed against GCC's.
        assert!(dumped.text.contains("#define __x86_64__ 1\n"), "{}", dumped.text);
    }

    #[test]
    fn the_dialect_reaches_the_predefined_set() {
        let mut opts = options();
        opts.std = rucc_session::Std::C99;
        opts.gnu_extensions = false;
        let result = run(&opts, &[("/main.c", "__STDC_VERSION__ __STRICT_ANSI__\n")]);
        assert_eq!(result.text, "# 1 \"/main.c\"\n199901L 1\n");
    }
}
