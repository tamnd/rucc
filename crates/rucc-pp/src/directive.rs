//! The directive engine: translation phase 4 over one file's preprocessing tokens.
//!
//! Design: `spec/05-preprocessor.md` section 5.4.
//!
//! A directive is a line whose first token is `#`. That is the whole of the recognition rule,
//! and the two halves of it are both load bearing: `#` has to be first on the line, and the
//! line is what the lexer says it is after splices and comments have been resolved, which is
//! why `x /*\n*/ #define F 1` really does define `F`.
//!
//! The part that is easy to get wrong is skipped regions. Inside `#if 0` a line beginning with
//! `#` still has to be recognised well enough to keep the conditional nesting balanced, and it
//! must not be diagnosed for anything else. Real code puts prose, unbalanced quotes and future
//! syntax inside `#if 0`, and a preprocessor that reports errors from there is unusable. So
//! skipping looks at the directive name and nothing else, and only the seven conditional
//! directives mean anything while it is going on.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rucc_base::{Interner, Symbol};
use rucc_diag::{Diagnostic, FileId, SourceMapFull, Span};
use rucc_gnu::Kind;
use rucc_lex::{Options, PpToken, PpTokenKind, Punct, TokenFlags, tokenize};
use rucc_session::{Found, IncludeForm};
use rucc_target::TargetInfo;

use crate::cond;
use crate::expand::Expander;
use crate::include::{
    Context, Frame, Header, Reader, directory_of, header_from_token, header_from_tokens, spelling,
};
use crate::macros::{Builtin, MacroTable, parse_define};
use crate::predef::{BUILT_IN, COMMAND_LINE, Predef, built_in, command_line};
use crate::token::Tok;

/// Why a file that has already been read does not need reading again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Guard {
    /// `#pragma once`, so the file is read once however many times it is named.
    Once,
    /// The whole file is wrapped in `#ifndef NAME`, and `NAME` is now defined, so reading it
    /// again would produce nothing at all. This is the multiple-include optimization, and on
    /// a real code base it is the difference between reading a header once and reading it a
    /// few hundred times.
    Macro(Symbol),
}

/// How far through the file the guard shape has been recognised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scan {
    /// Nothing has been seen yet, so the next line may open the guard.
    Start,
    /// Inside the conditional the file opened with.
    Inside(Symbol),
    /// The conditional closed and the file has to end here for the shape to hold.
    Closed(Symbol),
    /// Something else was seen, so this file has no guard.
    No,
}

/// One `#if` and everything hanging off it.
#[derive(Debug)]
struct Cond {
    /// Where the `#if` was written, so an unterminated one can point at it.
    span: Span,
    /// Whether tokens in the branch currently open are kept. Already accounts for whether the
    /// enclosing region was live, so [`Preprocessor::live`] only has to look at the top.
    live: bool,
    /// Whether some branch of this chain has been taken. A later `#elif` is not evaluated once
    /// this is set, which is what makes `#elif 1/0` after a taken branch legal.
    taken: bool,
    /// Whether the enclosing region was live.
    enclosing_live: bool,
    /// Whether `#else` has been seen, so a second one is an error.
    seen_else: bool,
}

/// A `#line` directive, kept for the source map to apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineDirective {
    /// Where the directive is.
    pub span: Span,
    /// The line number the next line is to be called.
    pub line: u32,
    /// The file name the following lines are to be called, if one was given.
    pub file: Option<Symbol>,
}

/// Translation phase 4 over one file.
///
/// Holds the macro table and the conditional stack, so a single instance processes a whole
/// translation unit and the definitions a header makes are visible after it.
#[derive(Debug, Default)]
pub struct Preprocessor {
    macros: MacroTable,
    expander: Expander,
    diagnostics: Vec<Diagnostic>,
    conds: Vec<Cond>,
    lines: Vec<LineDirective>,
    /// The files currently open, innermost last. Empty between runs.
    stack: Vec<Frame>,
    /// Files that do not need reading again, and why.
    seen: HashMap<PathBuf, Guard>,
}

impl Preprocessor {
    /// A preprocessor with an empty macro table.
    pub fn new() -> Preprocessor {
        Preprocessor::default()
    }

    /// The macros defined so far.
    pub fn macros(&self) -> &MacroTable {
        &self.macros
    }

    /// The macro table, for the driver to seed with `-D` and the predefined set.
    pub fn macros_mut(&mut self) -> &mut MacroTable {
        &mut self.macros
    }

    /// Everything reported so far.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Takes the diagnostics, leaving the preprocessor able to carry on.
    pub fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }

    /// The `#line` directives seen, in the order they appeared.
    ///
    /// They are recorded rather than applied. Applying one means the source map presenting a
    /// different name and a different line for a stretch of a file it holds the real ones
    /// for, and `__LINE__` and `__FILE__` reading the presented pair rather than the real
    /// one. That is a change to the map rather than to the preprocessor, and it lands with
    /// the `-E` output work that needs the same machinery for line markers.
    pub fn line_directives(&self) -> &[LineDirective] {
        &self.lines
    }

    /// Defines the predefined macro set, and then `-D` and `-U` from the command line.
    ///
    /// Called before [`Preprocessor::run`], because a predefined macro is a macro like any
    /// other by the time the source file is read. The set arrives as two synthetic files
    /// rather than as a list of definitions, so a diagnostic about one of them points at
    /// `<built-in>` or `<command-line>` the way GCC's does, and so that `-dM` has something
    /// to print. The reasoning is in `crate::predef`.
    ///
    /// # Errors
    ///
    /// When the source map has no room left for the two synthetic files.
    pub fn predefine(
        &mut self,
        target: &TargetInfo,
        opts: &Predef,
        cx: &mut Context<'_>,
    ) -> Result<(), SourceMapFull> {
        let names = Names::new(cx.interner);
        let file = self.synthetic(BUILT_IN, built_in(target, opts), cx, &names)?;
        // The macros that cannot be written as a `#define` line, because what they stand for
        // depends on where they are used. They go in after the generated file and before the
        // command line, so that `-U__FILE__` takes one away the way it takes any other away.
        // The origin is the start of `<built-in>`, which is where a warning about redefining
        // one points, and which is the truthful answer to where they came from.
        let start = cx.sources.file(file).start;
        for (spelling, builtin) in Builtin::ALL {
            let name = cx.interner.intern(spelling);
            self.macros.define_builtin(name, builtin, Span::new(start, start));
        }
        let text = command_line(opts);
        if !text.is_empty() {
            self.synthetic(COMMAND_LINE, text, cx, &names)?;
        }
        Ok(())
    }

    /// Reads a file the compiler wrote rather than one the user did.
    fn synthetic(
        &mut self,
        name: &str,
        text: String,
        cx: &mut Context<'_>,
        names: &Names,
    ) -> Result<FileId, SourceMapFull> {
        let file = cx.sources.add(name, text.into_bytes())?;
        let mut out = Vec::new();
        // A frame, so that the guard scan and the include depth see the same shape they see
        // for a real file. There is no directory, because `#include "x.h"` written in a
        // synthetic file has nowhere of its own to look.
        self.stack.push(Frame { at: Span::DUMMY, path: PathBuf::from(name), dir: None, next: 0 });
        self.process(file, &mut out, cx, names);
        self.stack.clear();
        debug_assert!(out.is_empty(), "{name} is directives only and produces no tokens");
        Ok(file)
    }

    /// Runs phase 4 over `file` and everything it includes.
    ///
    /// The result is the tokens that survived the conditionals, with macros expanded. Nothing
    /// is thrown away silently: an unterminated `#if` and a stray `#endif` are both reported.
    pub fn run(&mut self, file: FileId, cx: &mut Context<'_>) -> Vec<Tok> {
        let names = Names::new(cx.interner);
        let mut out = Vec::new();
        let name = cx.sources.file(file).name.clone();
        let dir = directory_of(&name);
        // The file named on the command line was not found through the search path, so an
        // `#include_next` written in it starts at the top rather than partway down.
        self.stack.push(Frame { at: Span::DUMMY, path: PathBuf::from(name), dir, next: 0 });
        self.process(file, &mut out, cx, &names);
        self.stack.clear();
        out
    }

    /// Reads one file, appending what survives to `out`.
    fn process(&mut self, file: FileId, out: &mut Vec<Tok>, cx: &mut Context<'_>, names: &Names) {
        // The bytes are taken out of the map by sharing rather than by borrowing, because the
        // rest of this function needs the map back to add an included file to it.
        let bytes = cx.sources.file(file).shared_bytes();
        let start = cx.sources.file(file).start;
        let mut reader = Reader::new(bytes.as_slice(), start, cx.lex);
        let depth_on_entry = self.conds.len();
        // Consecutive text lines are expanded as one run rather than line by line, because a
        // function-like macro invocation may span lines. It may not span a directive, which is
        // undefined behaviour, so a directive is where the run ends.
        let mut text: Vec<Tok> = Vec::new();
        let mut body: Vec<PpToken> = Vec::new();
        let mut scan = Scan::Start;

        loop {
            let was_live = self.live();
            let first = reader.next(cx.interner);
            if first.is_eof() {
                break;
            }
            if is_directive(first) {
                self.flush(&mut text, out, cx, names);
                body.clear();
                let name_tok = reader.next(cx.interner);
                // The null directive. A line of just `#` is legal and does nothing, and there
                // is a surprising amount of it in real headers as a visual separator.
                if name_tok.is_eof() || name_tok.flags.has(TokenFlags::START_OF_LINE) {
                    reader.put_back(name_tok);
                    continue;
                }
                body.push(name_tok);
                // The header name has to be scanned here or not at all: `<stdio.h>` and a run
                // of comparisons are the same bytes, and once the line has been scanned the
                // other way the difference is gone. Not in a skipped region, because scanning
                // one there can report an unterminated name that nobody asked about.
                if was_live && is_include(ident_of(&name_tok), names) {
                    if let Some(header) = reader.header_name(cx.interner) {
                        body.push(header);
                    }
                }
                reader.line(cx.interner, &mut body);
                let opens =
                    matches!(scan, Scan::Start).then(|| guard_opener(&body, names)).flatten();
                self.directive(&body, first.span, out, cx, names);
                scan = match scan {
                    // The guard has to be the first line of the file and it has to open a
                    // conditional, which is why the depth is checked after the dispatch
                    // rather than the directive name being trusted on its own.
                    Scan::Start => match opens {
                        Some(name) if self.conds.len() == depth_on_entry + 1 => Scan::Inside(name),
                        _ => Scan::No,
                    },
                    Scan::Inside(name) if self.conds.len() == depth_on_entry => Scan::Closed(name),
                    Scan::Inside(name) => Scan::Inside(name),
                    Scan::Closed(_) | Scan::No => Scan::No,
                };
            } else {
                body.clear();
                reader.line(cx.interner, &mut body);
                if self.live() {
                    text.push(Tok::new(first));
                    text.extend(body.iter().copied().map(Tok::new));
                }
                // A token outside the guard is a token that would be produced twice.
                if !matches!(scan, Scan::Inside(_)) {
                    scan = Scan::No;
                }
            }
            // What the lexer complained about while reading that line. A skipped region keeps
            // its complaints to itself, for the same reason it keeps its directives to itself.
            let complaints = reader.take_diagnostics();
            if was_live || self.live() {
                self.diagnostics.extend(complaints);
            }
        }
        self.flush(&mut text, out, cx, names);
        self.diagnostics.extend(reader.take_diagnostics());

        // The guard only counts if the macro really did get defined. A file that opens with
        // `#ifndef X` and never defines `X` is a file that has to be read again.
        if let Scan::Closed(name) = scan {
            if self.macros.is_defined(name) {
                if let Some(frame) = self.stack.last() {
                    self.seen.entry(frame.path.clone()).or_insert(Guard::Macro(name));
                }
            }
        }

        // A file may not close a conditional it did not open. GCC reports this at the `#if`,
        // which is the line the user has to go and look at.
        for cond in self.conds.drain(depth_on_entry..) {
            self.diagnostics
                .push(Diagnostic::error("unterminated `#if`", cond.span).with_code("E0330"));
        }
    }

    /// Whether tokens are currently being kept.
    fn live(&self) -> bool {
        self.conds.last().is_none_or(|c| c.live)
    }

    /// Expands a run of text lines and appends it to the output.
    fn flush(
        &mut self,
        text: &mut Vec<Tok>,
        out: &mut Vec<Tok>,
        cx: &mut Context<'_>,
        names: &Names,
    ) {
        if text.is_empty() {
            return;
        }
        let taken = std::mem::take(text);
        let expanded = self.expander.expand_toks(taken, &self.macros, cx.interner, cx.sources);
        self.diagnostics.append(&mut self.expander.take_diagnostics());
        self.pragma_operator(expanded, out, cx.interner, names);
    }

    /// Dispatches one directive. `body` is the line after the `#`.
    fn directive(
        &mut self,
        body: &[PpToken],
        hash: Span,
        out: &mut Vec<Tok>,
        cx: &mut Context<'_>,
        names: &Names,
    ) {
        let Some(first) = body.first().copied() else {
            return;
        };
        let name = ident_of(&first);
        let rest = &body[1..];

        // Conditionals are handled whether or not the region is live, because the nesting has
        // to stay balanced through a skipped block.
        if name == Some(names.r#if) {
            let value = self.live() && self.eval(rest, hash, cx, names);
            self.open(hash, value);
            return;
        }
        if name == Some(names.ifdef) || name == Some(names.ifndef) {
            let want = name == Some(names.ifdef);
            let value = self.live() && self.defined_check(rest, hash, want, names);
            self.open(hash, value);
            return;
        }
        if name == Some(names.elif) || name == Some(names.elifdef) || name == Some(names.elifndef) {
            self.elif(name, rest, hash, cx, names);
            return;
        }
        if name == Some(names.r#else) {
            self.branch_else(rest, hash);
            return;
        }
        if name == Some(names.endif) {
            self.endif(rest, hash);
            return;
        }
        if !self.live() {
            // Everything else inside a skipped region is text, not a directive. `#error` in
            // the branch that was not taken must not fire, and `# 42 "f.c"` from another
            // preprocessor must not be diagnosed.
            return;
        }

        let interner = &mut *cx.interner;
        if name == Some(names.define) {
            let (def, diagnostics) = parse_define(rest, interner);
            self.diagnostics.extend(diagnostics);
            if let Some(def) = def {
                if let Some(problem) = self.macros.define(def, interner) {
                    self.diagnostics.push(problem);
                }
            }
        } else if name == Some(names.undef) {
            self.undef(rest, hash, interner);
        } else if name == Some(names.error) || name == Some(names.warning) {
            self.message(rest, hash, name == Some(names.error), interner);
        } else if name == Some(names.line) {
            self.line(rest, hash, cx);
        } else if name == Some(names.pragma) {
            // `#pragma once` is answered here and does not reach the output, because it is a
            // question about the file rather than something a later phase can act on.
            // Everything else is passed through unchanged, which is what `-E` has to print
            // and what a later phase looking for `#pragma pack` will read. Inventing an
            // internal representation now, with no consumer, would only be a thing to
            // migrate later.
            if rest.len() == 1 && ident_of(&rest[0]) == Some(names.once) {
                self.pragma_once(hash);
            } else {
                self.pass_through(body, hash, out);
            }
        } else if name == Some(names.include) || name == Some(names.include_next) {
            self.include(rest, hash, name == Some(names.include_next), out, cx, names);
        } else if name == Some(names.embed) {
            // Recognised so that the line is not reported as an unknown directive, and
            // refused so that nobody mistakes silence for a working `#embed`. It needs the
            // fast path in the parser described in `spec/05-preprocessor.md` section 5.4, and
            // there is no parser yet.
            self.diagnostics.push(
                Diagnostic::error("`#embed` is not implemented yet", hash)
                    .with_code("E0331")
                    .note("see spec/05-preprocessor.md section 5.4", hash),
            );
        } else {
            self.diagnostics.push(
                Diagnostic::error("invalid preprocessing directive", first.span).with_code("E0332"),
            );
        }
    }

    /// Records that the file currently being read asked to be read only once.
    fn pragma_once(&mut self, hash: Span) {
        // A file that is not included cannot be included twice, so the line is more likely to
        // be a mistake than a no-op. GCC says the same thing.
        if self.stack.len() <= 1 {
            self.diagnostics.push(
                Diagnostic::warning("`#pragma once` in the main file", hash).with_code("W0332"),
            );
            return;
        }
        if let Some(frame) = self.stack.last() {
            self.seen.insert(frame.path.clone(), Guard::Once);
        }
    }

    /// Whether a file has already given everything it has to give.
    fn skip(&self, path: &Path) -> bool {
        match self.seen.get(path) {
            Some(Guard::Once) => true,
            Some(Guard::Macro(name)) => self.macros.is_defined(*name),
            None => false,
        }
    }

    /// Copies a directive line into the output, `#` included.
    fn pass_through(&mut self, body: &[PpToken], hash: Span, out: &mut Vec<Tok>) {
        let _ = self;
        out.push(Tok::synthetic(
            PpTokenKind::Punct(Punct::Hash),
            None,
            TokenFlags::START_OF_LINE,
            hash,
        ));
        out.extend(body.iter().copied().map(Tok::new));
    }

    /// Resolves an `#include` or `#include_next` and reads what it names.
    fn include(
        &mut self,
        rest: &[PpToken],
        hash: Span,
        is_next: bool,
        out: &mut Vec<Tok>,
        cx: &mut Context<'_>,
        names: &Names,
    ) {
        let Some(header) = self.header_of(rest, hash, cx) else {
            return;
        };
        let (form, relative_to, from) = self.where_to_look(&header, is_next, cx);
        let found = cx.search.resolve(cx.fs, &header.name, form, relative_to.as_deref(), from);
        let Some(found) = found else {
            let tried = cx.search.tried(&header.name, form, relative_to.as_deref(), from);
            let where_looked = if tried.is_empty() {
                "the name is an absolute path, so the search path was not used".to_owned()
            } else {
                let list: Vec<String> =
                    tried.iter().map(|d| d.to_string_lossy().into_owned()).collect();
                format!("searched: {}", list.join(", "))
            };
            self.diagnostics.push(
                Diagnostic::error(format!("`{}` file not found", header.name), hash)
                    .with_code("E0341")
                    .note(where_looked, hash),
            );
            return;
        };
        // The multiple-include optimization. A file wrapped in an include guard whose macro
        // is now defined, or one that asked for `#pragma once`, would produce nothing, so it
        // is not opened at all. On a real code base this is the difference between reading a
        // header once and reading it a few hundred times.
        if self.skip(&found.path) {
            return;
        }
        if self.stack.len() >= cx.max_include_depth as usize {
            let mut diagnostic =
                Diagnostic::error("`#include` nested too deeply", hash).with_code("E0342").note(
                    "a header that includes itself with no include guard is the usual cause",
                    hash,
                );
            if let Some(outer) = self.stack.first().filter(|f| !f.at.is_dummy()) {
                diagnostic = diagnostic.note("the outermost include is here", outer.at);
            }
            self.diagnostics.push(diagnostic);
            return;
        }
        let added = cx.sources.add_shared(found.name.clone(), found.bytes.clone(), Some(hash));
        let file = match added {
            Ok(file) => file,
            Err(full) => {
                self.diagnostics.push(Diagnostic::error(full.to_string(), hash).with_code("E0344"));
                return;
            }
        };
        self.stack.push(Frame {
            at: hash,
            dir: found.path.parent().map(Path::to_path_buf),
            path: found.path,
            next: found.next,
        });
        self.process(file, out, cx, names);
        self.stack.pop();
    }

    /// Where a header written in the file being read is looked for.
    ///
    /// `#include_next` continues from the directory after the one the current file came from,
    /// which is what glibc and the kernel use to wrap a system header with one of the same
    /// name. It never looks next to the current file, because that directory is not on the
    /// path and there would be nothing to continue past.
    ///
    /// `__has_include` has to ask the same question the directive would, so both go through
    /// here. A header that answers yes and then fails to be found is the one outcome that
    /// would make the operator useless.
    fn where_to_look(
        &self,
        header: &Header,
        is_next: bool,
        cx: &Context<'_>,
    ) -> (IncludeForm, Option<PathBuf>, usize) {
        let form = if header.angled { IncludeForm::Angled } else { IncludeForm::Quoted };
        let frame = self.stack.last();
        let from = if is_next {
            frame.map_or(0, |f| f.next).max(cx.search.start(form))
        } else {
            cx.search.start(form)
        };
        let relative_to = if is_next { None } else { frame.and_then(|f| f.dir.clone()) };
        (form, relative_to, from)
    }

    /// Whether a header is there, which is all `__has_include` asks.
    fn find(&self, header: &Header, is_next: bool, cx: &Context<'_>) -> Option<Found> {
        let (form, relative_to, from) = self.where_to_look(header, is_next, cx);
        cx.search.resolve(cx.fs, &header.name, form, relative_to.as_deref(), from)
    }

    /// The header name an include directive names, however it spelled it.
    fn header_of(&mut self, rest: &[PpToken], hash: Span, cx: &mut Context<'_>) -> Option<Header> {
        if let Some(first) = rest.first().copied() {
            if first.kind == PpTokenKind::HeaderName {
                let text = first.value.map_or("", |v| cx.interner.resolve(v));
                let header = header_from_token(text);
                if header.is_none() {
                    self.bad_header(first.span);
                }
                self.extra_tokens(&rest[1..], "#include");
                return header;
            }
        }
        // The computed include, `#include MACRO`. The line is macro expanded and then has to
        // look like a header name, which is the one place in the language where the spelling
        // of a token matters after expansion.
        if rest.is_empty() {
            self.bad_header(hash);
            return None;
        }
        let line: Vec<Tok> = rest.iter().copied().map(Tok::new).collect();
        let expanded = self.expander.expand_toks(line, &self.macros, cx.interner, cx.sources);
        self.diagnostics.append(&mut self.expander.take_diagnostics());
        let spellings: Vec<&str> = expanded.iter().map(|t| spelling(*t, cx.interner)).collect();
        let header = header_from_tokens(&spellings);
        if header.is_none() {
            let at = expanded.first().map_or(hash, |t| t.report_span());
            self.bad_header(at);
        }
        header
    }

    fn bad_header(&mut self, at: Span) {
        self.diagnostics.push(
            Diagnostic::error("expected a file name in `<>` or `\"\"`", at).with_code("E0343"),
        );
    }

    /// Pushes a conditional whose first branch is or is not taken.
    fn open(&mut self, span: Span, value: bool) {
        let enclosing_live = self.live();
        self.conds.push(Cond {
            span,
            live: enclosing_live && value,
            taken: value,
            enclosing_live,
            seen_else: false,
        });
    }

    fn elif(
        &mut self,
        name: Option<Symbol>,
        rest: &[PpToken],
        hash: Span,
        cx: &mut Context<'_>,
        names: &Names,
    ) {
        let Some(top) = self.conds.last() else {
            self.stray("elif", hash);
            return;
        };
        if top.seen_else {
            self.diagnostics
                .push(Diagnostic::error("`#elif` after `#else`", hash).with_code("E0333"));
            return;
        }
        // Read what is needed before evaluating, because evaluation borrows the whole
        // preprocessor to report into.
        let (enclosing_live, already_taken) = (top.enclosing_live, top.taken);
        let consider = enclosing_live && !already_taken;
        let value = if !consider {
            false
        } else if name == Some(names.elif) {
            self.eval(rest, hash, cx, names)
        } else {
            self.defined_check(rest, hash, name == Some(names.elifdef), names)
        };
        let top = self.conds.last_mut().expect("checked above and nothing popped");
        top.live = consider && value;
        top.taken = already_taken || value;
    }

    fn branch_else(&mut self, rest: &[PpToken], hash: Span) {
        let Some(top) = self.conds.last_mut() else {
            self.stray("else", hash);
            return;
        };
        if top.seen_else {
            self.diagnostics.push(Diagnostic::error("a second `#else`", hash).with_code("E0333"));
            return;
        }
        top.live = top.enclosing_live && !top.taken;
        top.taken = true;
        top.seen_else = true;
        let enclosing_live = top.enclosing_live;
        if enclosing_live {
            self.extra_tokens(rest, "#else");
        }
    }

    fn endif(&mut self, rest: &[PpToken], hash: Span) {
        if self.conds.pop().is_none() {
            self.stray("endif", hash);
            return;
        }
        if self.live() {
            self.extra_tokens(rest, "#endif");
        }
    }

    fn stray(&mut self, what: &str, hash: Span) {
        self.diagnostics
            .push(Diagnostic::error(format!("`#{what}` without `#if`"), hash).with_code("E0334"));
    }

    /// Warns about tokens after a directive that takes none.
    ///
    /// A warning rather than an error, because `#endif FOO` as a hand written comment is
    /// everywhere in code written before `//` was portable.
    fn extra_tokens(&mut self, rest: &[PpToken], what: &str) {
        if let Some(first) = rest.first() {
            self.diagnostics.push(
                Diagnostic::warning(format!("extra tokens after `{what}`"), first.span)
                    .with_code("W0330"),
            );
        }
    }

    /// Evaluates a `#if` or `#elif` expression.
    fn eval(&mut self, rest: &[PpToken], hash: Span, cx: &mut Context<'_>, names: &Names) -> bool {
        let line: Vec<Tok> = rest.iter().copied().map(Tok::new).collect();
        // `defined X` is resolved before expansion, so that `#if defined FOO` does not depend
        // on what `FOO` expands to. It is resolved again afterwards because a macro that
        // expands to `defined(X)` is undefined behaviour that GCC supports and headers use.
        // It goes first of all because `defined(__has_include)` is a question about the
        // operator rather than a use of it.
        let line = self.resolve_defined(line, cx.interner, names);
        // `__has_include` is resolved before expansion too, and for a stronger reason: its
        // operand is a header name, so expanding `<linux/version.h>` would turn `linux` into
        // `1` on a target where that macro is predefined. The rest of the family take an
        // identifier that GCC does expand, so they wait until afterwards.
        let line = self.resolve_has(line, cx, names, true);
        let line = self.expander.expand_toks(line, &self.macros, cx.interner, cx.sources);
        self.diagnostics.append(&mut self.expander.take_diagnostics());
        let line = self.resolve_defined(line, cx.interner, names);
        let line = self.resolve_has(line, cx, names, false);
        cond::evaluate(&line, cx.interner, &mut self.diagnostics, hash)
    }

    /// Replaces `__has_include(<x.h>)` and the rest of the family with what they answer.
    ///
    /// `headers_only` is the pass before macro expansion, which resolves the two operators
    /// whose operand must not be expanded and leaves the others alone.
    fn resolve_has(
        &mut self,
        line: Vec<Tok>,
        cx: &mut Context<'_>,
        names: &Names,
        headers_only: bool,
    ) -> Vec<Tok> {
        if !line.iter().any(|t| t.ident().is_some_and(|n| names.has.op(n).is_some())) {
            return line;
        }
        let mut out = Vec::with_capacity(line.len());
        let mut at = 0;
        while at < line.len() {
            let tok = line[at];
            let op = tok.ident().and_then(|n| names.has.op(n));
            let Some(op) = op.filter(|op| !headers_only || op.is_header()) else {
                out.push(tok);
                at += 1;
                continue;
            };
            let Some((operand, after)) = arguments(&line, at + 1) else {
                // Reported in the pass after expansion and not in the one before it, because
                // the operator is still there for that pass to find and one mistake is one
                // diagnostic.
                if !headers_only {
                    self.diagnostics.push(
                        Diagnostic::error(
                            format!("expected `(` after `{}`", spelling(tok, cx.interner)),
                            tok.report_span(),
                        )
                        .with_code("E0345"),
                    );
                }
                out.push(tok);
                at += 1;
                continue;
            };
            at = after;
            // A number rather than a flag, because `__has_c_attribute` answers with the value
            // the standard gives the attribute and a header compares that against a date.
            let value = self.ask(op, operand, tok, cx);
            let sym = cx.interner.intern(&value.to_string());
            out.push(Tok::synthetic(PpTokenKind::Number, Some(sym), tok.flags, tok.report_span()));
        }
        out
    }

    /// What one `__has_*` operator answers for one operand.
    fn ask(&mut self, op: Op, operand: &[Tok], tok: Tok, cx: &mut Context<'_>) -> u32 {
        let at = operand.first().map_or(tok.report_span(), |t| t.report_span());
        match op {
            Op::Include | Op::IncludeNext => {
                let spellings: Vec<&str> =
                    operand.iter().map(|t| spelling(*t, cx.interner)).collect();
                let Some(header) = header_from_tokens(&spellings) else {
                    self.bad_header(at);
                    return 0;
                };
                u32::from(self.find(&header, op == Op::IncludeNext, cx).is_some())
            }
            Op::Table(kind) => {
                let Some(name) = attribute_name(operand, cx.interner) else {
                    self.diagnostics.push(
                        Diagnostic::error(
                            format!(
                                "expected an identifier as the operand of `{}`",
                                spelling(tok, cx.interner)
                            ),
                            at,
                        )
                        .with_code("E0345"),
                    );
                    return 0;
                };
                match kind {
                    Kind::Attribute => rucc_gnu::has_attribute(name),
                    Kind::CAttribute => rucc_gnu::has_c_attribute(name),
                    Kind::Builtin => rucc_gnu::has_builtin(name),
                    Kind::Feature => rucc_gnu::has_feature(name),
                    Kind::Extension => rucc_gnu::has_extension(name),
                }
            }
        }
    }

    /// Replaces `defined X` and `defined(X)` with `1` or `0`.
    fn resolve_defined(
        &mut self,
        line: Vec<Tok>,
        interner: &mut Interner,
        names: &Names,
    ) -> Vec<Tok> {
        if !line.iter().any(|t| t.ident() == Some(names.defined)) {
            return line;
        }
        let mut out = Vec::with_capacity(line.len());
        let mut at = 0;
        while at < line.len() {
            let tok = line[at];
            if tok.ident() != Some(names.defined) {
                out.push(tok);
                at += 1;
                continue;
            }
            let parenthesised = line.get(at + 1).is_some_and(|t| t.is(Punct::LParen));
            let name_at = if parenthesised { at + 2 } else { at + 1 };
            let name = line.get(name_at).and_then(|t| t.ident());
            let Some(name) = name else {
                self.diagnostics.push(
                    Diagnostic::error("`defined` without a macro name", tok.report_span())
                        .with_code("E0335"),
                );
                out.push(tok);
                at += 1;
                continue;
            };
            at = name_at + 1;
            if parenthesised {
                if line.get(at).is_some_and(|t| t.is(Punct::RParen)) {
                    at += 1;
                } else {
                    self.diagnostics.push(
                        Diagnostic::error("expected `)` after `defined`", tok.report_span())
                            .with_code("E0335"),
                    );
                }
            }
            // A header asks `#ifdef __has_include` before using it, because the operator is
            // newer than some of the compilers it has to build under. It is not a macro, but
            // the question being asked is whether the name means something, and it does.
            let value = self.macros.is_defined(name) || names.has.op(name).is_some();
            out.push(number(value, tok.flags, tok.report_span(), interner));
        }
        out
    }

    /// The body of `#ifdef`, `#ifndef`, `#elifdef` and `#elifndef`.
    fn defined_check(
        &mut self,
        rest: &[PpToken],
        hash: Span,
        want_defined: bool,
        names: &Names,
    ) -> bool {
        let Some(name) = rest.first().and_then(ident_of) else {
            self.diagnostics.push(
                Diagnostic::error("expected a macro name", rest.first().map_or(hash, |t| t.span))
                    .with_code("E0336"),
            );
            return false;
        };
        self.extra_tokens(&rest[1..], if want_defined { "#ifdef" } else { "#ifndef" });
        let defined = self.macros.is_defined(name) || names.has.op(name).is_some();
        defined == want_defined
    }

    fn undef(&mut self, rest: &[PpToken], hash: Span, interner: &Interner) {
        let Some(name) = rest.first().and_then(ident_of) else {
            self.diagnostics.push(
                Diagnostic::error("expected a macro name", rest.first().map_or(hash, |t| t.span))
                    .with_code("E0336"),
            );
            return;
        };
        // The standard reserves these and GCC refuses to let them go, because code that
        // undefines `__FILE__` and then uses it is broken in a way that is very hard to see.
        let text = interner.resolve(name);
        if text == "defined" || text.starts_with("__STDC_") {
            self.diagnostics.push(
                Diagnostic::error(format!("`{text}` cannot be undefined"), rest[0].span)
                    .with_code("E0337"),
            );
            return;
        }
        self.macros.undef(name);
        self.extra_tokens(&rest[1..], "#undef");
    }

    /// `#error` and `#warning`. The message is the rest of the line, spelled back.
    fn message(&mut self, rest: &[PpToken], hash: Span, fatal: bool, interner: &Interner) {
        let text = spell_line(rest, interner);
        let span = rest.first().map_or(hash, |t| t.span.to(last_span(rest)));
        let diag = if fatal {
            Diagnostic::error(text, span).with_code("E0338")
        } else {
            Diagnostic::warning(text, span).with_code("W0331")
        };
        self.diagnostics.push(diag);
    }

    /// `#line 42` and `#line 42 "file.c"`.
    ///
    /// The argument is macro expanded first, which is the one place a directive other than
    /// `#if` does that, and which exists because `#line __LINE__ + 1` is real code.
    fn line(&mut self, rest: &[PpToken], hash: Span, cx: &mut Context<'_>) {
        let line: Vec<Tok> = rest.iter().copied().map(Tok::new).collect();
        let line = self.expander.expand_toks(line, &self.macros, cx.interner, cx.sources);
        self.diagnostics.append(&mut self.expander.take_diagnostics());
        let interner = &mut *cx.interner;

        let number_text = line
            .first()
            .filter(|t| t.kind == PpTokenKind::Number)
            .and_then(|t| t.value)
            .map(|v| interner.resolve(v));
        let Some(parsed) = number_text.and_then(|t| t.parse::<u64>().ok()) else {
            self.diagnostics.push(
                Diagnostic::error(
                    "`#line` needs a decimal line number",
                    line.first().map_or(hash, |t| t.report_span()),
                )
                .with_code("E0339"),
            );
            return;
        };
        // 2147483647 is the largest line number the standard requires support for, and it is
        // also where every other compiler stops, so matching that keeps diagnostics comparable.
        if parsed == 0 || parsed > 2_147_483_647 {
            self.diagnostics.push(
                Diagnostic::error("`#line` number is out of range", line[0].report_span())
                    .with_code("E0339"),
            );
            return;
        }

        let mut file = None;
        if let Some(second) = line.get(1) {
            if second.kind == PpTokenKind::StringLit {
                file = second.value;
            } else {
                self.diagnostics.push(
                    Diagnostic::error(
                        "`#line` file name must be a string literal",
                        second.report_span(),
                    )
                    .with_code("E0339"),
                );
                return;
            }
        }
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the range check above keeps this inside i32, let alone u32"
        )]
        self.lines.push(LineDirective { span: hash, line: parsed as u32, file });
    }

    /// Applies the `_Pragma` operator to an expanded run and appends the result.
    ///
    /// `_Pragma("x")` is a pragma written as an expression, which is what makes a pragma
    /// usable from inside a macro. It is handled after expansion because the string it takes
    /// is very often produced by one.
    fn pragma_operator(
        &mut self,
        expanded: Vec<Tok>,
        out: &mut Vec<Tok>,
        interner: &mut Interner,
        names: &Names,
    ) {
        if !expanded.iter().any(|t| t.ident() == Some(names.pragma_op)) {
            out.extend(expanded);
            return;
        }
        let mut at = 0;
        while at < expanded.len() {
            let tok = expanded[at];
            if tok.ident() != Some(names.pragma_op) {
                out.push(tok);
                at += 1;
                continue;
            }
            let open = expanded.get(at + 1).is_some_and(|t| t.is(Punct::LParen));
            let text = expanded.get(at + 2).filter(|t| t.kind == PpTokenKind::StringLit);
            let close = expanded.get(at + 3).is_some_and(|t| t.is(Punct::RParen));
            let (Some(text), true, true) = (text, open, close) else {
                self.diagnostics.push(
                    Diagnostic::error("`_Pragma` takes a single string literal", tok.report_span())
                        .with_code("E0340"),
                );
                out.push(tok);
                at += 1;
                continue;
            };
            let literal = text.value.map(|v| interner.resolve(v)).unwrap_or_default();
            let body = destringize(literal);
            self.emit_pragma(&body, tok, out, interner, names);
            at += 4;
        }
    }

    /// Turns destringized `_Pragma` text into the `# pragma ...` tokens a later phase reads.
    fn emit_pragma(
        &mut self,
        body: &str,
        at: Tok,
        out: &mut Vec<Tok>,
        interner: &mut Interner,
        names: &Names,
    ) {
        let span = at.report_span();
        let (tokens, diagnostics) = tokenize(body.as_bytes(), 0, Options::new(), interner);
        // The text came out of a string literal, so a span into it would point at bytes the
        // user cannot see. Every token reports at the `_Pragma` instead.
        self.diagnostics.extend(
            diagnostics
                .into_iter()
                .map(|d| Diagnostic::new(d.severity, d.message, span).with_code("E0340")),
        );
        out.push(Tok::synthetic(
            PpTokenKind::Punct(Punct::Hash),
            None,
            TokenFlags::START_OF_LINE,
            span,
        ));
        out.push(Tok::synthetic(PpTokenKind::Ident, Some(names.pragma), TokenFlags::EMPTY, span));
        // The tokens keep the spacing they were written with inside the string, so
        // `_Pragma("pack(push)")` prints back as `pack(push)` rather than `pack ( push )`.
        // Only the first one is forced apart, from the `pragma` before it.
        for (at, t) in tokens.into_iter().filter(|t| !t.is_eof()).enumerate() {
            // Start of line has to come off: the line is the `#pragma` we just emitted, not
            // the inside of the string these came from.
            let spaced = at == 0 || t.flags.has(TokenFlags::LEADING_SPACE);
            let flags = if spaced {
                TokenFlags::EMPTY.with(TokenFlags::LEADING_SPACE)
            } else {
                TokenFlags::EMPTY
            };
            out.push(Tok::synthetic(t.kind, t.value, flags, span));
        }
    }
}

/// The macro a file's opening line guards the whole file with, if the line has that shape.
///
/// `#ifndef NAME` and both spellings of `#if !defined NAME`, which between them are what
/// every header in glibc, musl and the kernel is wrapped in.
fn guard_opener(body: &[PpToken], names: &Names) -> Option<Symbol> {
    let name = ident_of(body.first()?)?;
    let rest = &body[1..];
    if name == names.ifndef {
        let [only] = rest else {
            return None;
        };
        return ident_of(only);
    }
    if name != names.r#if {
        return None;
    }
    let [bang, defined, tail @ ..] = rest else {
        return None;
    };
    if bang.punct() != Some(Punct::Bang) || ident_of(defined) != Some(names.defined) {
        return None;
    }
    match tail {
        [only] => ident_of(only),
        [open, only, close]
            if open.punct() == Some(Punct::LParen) && close.punct() == Some(Punct::RParen) =>
        {
            ident_of(only)
        }
        _ => None,
    }
}

/// Whether a directive name is one that may be followed by a header name.
fn is_include(name: Option<Symbol>, names: &Names) -> bool {
    name == Some(names.include) || name == Some(names.include_next) || name == Some(names.embed)
}

/// Whether this token opens a directive line.
fn is_directive(tok: PpToken) -> bool {
    tok.flags.has(TokenFlags::START_OF_LINE) && tok.punct() == Some(Punct::Hash)
}

fn ident_of(tok: &PpToken) -> Option<Symbol> {
    match tok.kind {
        PpTokenKind::Ident => tok.value,
        _ => None,
    }
}

fn last_span(tokens: &[PpToken]) -> Span {
    tokens.last().map_or(Span::DUMMY, |t| t.span)
}

/// A synthetic `1` or `0`.
fn number(value: bool, flags: TokenFlags, span: Span, interner: &mut Interner) -> Tok {
    let sym = interner.intern(if value { "1" } else { "0" });
    Tok::synthetic(PpTokenKind::Number, Some(sym), flags, span)
}

/// Spells a directive's tokens back for an `#error` message.
fn spell_line(tokens: &[PpToken], interner: &Interner) -> String {
    let mut out = String::new();
    for (index, tok) in tokens.iter().enumerate() {
        if index > 0 && tok.flags.has(TokenFlags::LEADING_SPACE) {
            out.push(' ');
        }
        match tok.value {
            Some(sym) => out.push_str(interner.resolve(sym)),
            None => {
                if let Some(p) = tok.punct() {
                    out.push_str(p.as_str());
                }
            }
        }
    }
    out
}

/// Undoes what `#` would have done, per C23 6.10.10.
///
/// The `L` or `u8` prefix and the quotes come off, then `\"` becomes `"` and `\\` becomes `\`.
/// No other escape is touched, because no other escape was introduced.
fn destringize(literal: &str) -> String {
    let body = literal
        .trim_start_matches(['L', 'u', 'U', '8'])
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(literal);
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// The parenthesised operand of a `__has_*` operator, and where the line carries on.
///
/// `None` when the next token is not `(`, which is the only shape the operators take. Nesting
/// is counted rather than stopping at the first `)`, so that `__has_include(HEADER(x))` after
/// expansion still finds the end of its own operand.
fn arguments(line: &[Tok], at: usize) -> Option<(&[Tok], usize)> {
    if !line.get(at)?.is(Punct::LParen) {
        return None;
    }
    let mut depth = 1u32;
    let mut end = at + 1;
    while end < line.len() {
        if line[end].is(Punct::LParen) {
            depth += 1;
        } else if line[end].is(Punct::RParen) {
            depth -= 1;
            if depth == 0 {
                return Some((&line[at + 1..end], end + 1));
            }
        }
        end += 1;
    }
    None
}

/// The name `__has_attribute` and its relatives are asked about.
///
/// A bare identifier, or the scoped form `gnu::always_inline` that C23 gives the attributes
/// that came from GCC. The scope is dropped: `__has_c_attribute(gnu::x)` and
/// `__has_attribute(x)` are the same question, and the matrix has one row for it.
fn attribute_name<'i>(operand: &[Tok], interner: &'i Interner) -> Option<&'i str> {
    let name = match operand {
        [one] => one,
        [_, scope, name] if scope.is(Punct::ColonColon) => name,
        _ => return None,
    };
    name.ident().map(|sym| interner.resolve(sym))
}

/// Which `__has_*` operator a name is, and what answers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    /// `__has_include`, answered by looking for the header.
    Include,
    /// `__has_include_next`, the same question from further down the search path.
    IncludeNext,
    /// The rest of the family, answered out of the matrix in `rucc-gnu`.
    Table(Kind),
}

impl Op {
    /// Whether the operand is a header name, which must not be macro expanded.
    fn is_header(self) -> bool {
        matches!(self, Op::Include | Op::IncludeNext)
    }
}

/// The `__has_*` operators, interned once per file.
///
/// A short array rather than a map: there are seven of them, the comparison is on interned
/// symbols, and it is only reached for a line that mentions one.
struct HasOps {
    ops: [(Symbol, Op); 7],
}

impl HasOps {
    fn new(interner: &mut Interner) -> HasOps {
        HasOps {
            ops: [
                (interner.intern("__has_include"), Op::Include),
                (interner.intern("__has_include_next"), Op::IncludeNext),
                (interner.intern("__has_attribute"), Op::Table(Kind::Attribute)),
                (interner.intern("__has_c_attribute"), Op::Table(Kind::CAttribute)),
                (interner.intern("__has_builtin"), Op::Table(Kind::Builtin)),
                (interner.intern("__has_feature"), Op::Table(Kind::Feature)),
                (interner.intern("__has_extension"), Op::Table(Kind::Extension)),
            ],
        }
    }

    /// The operator a name is, if it is one.
    fn op(&self, name: Symbol) -> Option<Op> {
        self.ops.iter().find(|(sym, _)| *sym == name).map(|(_, op)| *op)
    }
}

/// The directive names and the two operators, interned once per file.
///
/// Comparing symbols rather than strings is the point: a directive line is recognised with
/// integer comparisons, and the identifiers were interned during the scan, so there is no
/// string work in the hot path.
struct Names {
    define: Symbol,
    undef: Symbol,
    r#if: Symbol,
    ifdef: Symbol,
    ifndef: Symbol,
    elif: Symbol,
    elifdef: Symbol,
    elifndef: Symbol,
    r#else: Symbol,
    endif: Symbol,
    line: Symbol,
    error: Symbol,
    warning: Symbol,
    pragma: Symbol,
    include: Symbol,
    include_next: Symbol,
    embed: Symbol,
    defined: Symbol,
    once: Symbol,
    pragma_op: Symbol,
    has: HasOps,
}

impl Names {
    fn new(interner: &mut Interner) -> Names {
        Names {
            define: interner.intern("define"),
            undef: interner.intern("undef"),
            r#if: interner.intern("if"),
            ifdef: interner.intern("ifdef"),
            ifndef: interner.intern("ifndef"),
            elif: interner.intern("elif"),
            elifdef: interner.intern("elifdef"),
            elifndef: interner.intern("elifndef"),
            r#else: interner.intern("else"),
            endif: interner.intern("endif"),
            line: interner.intern("line"),
            error: interner.intern("error"),
            warning: interner.intern("warning"),
            pragma: interner.intern("pragma"),
            include: interner.intern("include"),
            include_next: interner.intern("include_next"),
            embed: interner.intern("embed"),
            defined: interner.intern("defined"),
            once: interner.intern("once"),
            pragma_op: interner.intern("_Pragma"),
            has: HasOps::new(interner),
        }
    }
}

#[cfg(test)]
mod tests {
    use rucc_diag::{Severity, SourceMap};
    use rucc_session::{MemoryFileSystem, SearchPath};

    use super::*;
    use rucc_session::Std;

    use crate::predef::Timestamp;

    /// A whole file through phase 4, which is what almost every test here wants.
    ///
    /// The main file is always `/main.c`, so a quoted include with no search path set up
    /// finds a header the test put at `/name.h`.
    struct Run {
        interner: Interner,
        sources: SourceMap,
        fs: MemoryFileSystem,
        search: SearchPath,
        pp: Preprocessor,
    }

    impl Run {
        fn new() -> Run {
            Run {
                interner: Interner::new(),
                sources: SourceMap::new(),
                fs: MemoryFileSystem::new(),
                search: SearchPath::new(),
                pp: Preprocessor::new(),
            }
        }

        /// Puts a header where an include can find it.
        fn file(&mut self, path: &str, contents: &str) {
            self.fs.insert(path, contents.as_bytes().to_vec());
        }

        /// Adds a directory to the `-I` part of the search path.
        fn dir(&mut self, path: &str) {
            self.search.push_bracket(path);
        }

        /// Defines the predefined set for a target, as the driver does before reading input.
        fn predefine(&mut self, triple: &str, opts: &Predef) {
            let target = TargetInfo::new(triple.parse().expect("a supported triple"));
            let mut cx =
                Context::new(&mut self.interner, &mut self.sources, &self.fs, &self.search);
            self.pp.predefine(&target, opts, &mut cx).expect("the map has room");
        }

        /// The surviving tokens, spelled with one space wherever they were separated.
        fn go(&mut self, src: &str) -> String {
            self.go_named("/main.c", src)
        }

        /// The same, for a test that cares what the main file is called.
        fn go_named(&mut self, path: &str, src: &str) -> String {
            let file = self.sources.add(path, src.as_bytes().to_vec()).expect("the map has room");
            let out = {
                let mut cx =
                    Context::new(&mut self.interner, &mut self.sources, &self.fs, &self.search);
                self.pp.run(file, &mut cx)
            };
            let mut text = String::new();
            for (at, tok) in out.iter().enumerate() {
                let spaced = tok.flags.has(TokenFlags::LEADING_SPACE)
                    || tok.flags.has(TokenFlags::START_OF_LINE);
                if at > 0 && spaced {
                    text.push(' ');
                }
                match tok.kind {
                    PpTokenKind::Punct(p) => text.push_str(p.as_str()),
                    _ => text.push_str(
                        self.interner.resolve(tok.value.expect("every non-punctuator interns")),
                    ),
                }
            }
            text
        }

        /// How many files were opened, main file included. A header that the guard
        /// optimization skipped never reaches the source map, so this is what says whether
        /// it was really skipped rather than read and thrown away.
        fn files(&self) -> usize {
            self.sources.files().len()
        }

        fn messages(&mut self) -> Vec<String> {
            self.pp.take_diagnostics().into_iter().map(|d| d.message).collect()
        }

        fn severities(&mut self) -> Vec<Severity> {
            self.pp.diagnostics().iter().map(|d| d.severity).collect()
        }
    }

    fn clean(src: &str) -> String {
        let mut run = Run::new();
        let text = run.go(src);
        assert!(run.messages().is_empty(), "expected no diagnostics from {src:?}");
        text
    }

    #[test]
    fn a_taken_branch_is_kept_and_the_other_is_not() {
        assert_eq!(clean("#if 1\nyes\n#else\nno\n#endif\n"), "yes");
        assert_eq!(clean("#if 0\nyes\n#else\nno\n#endif\n"), "no");
    }

    #[test]
    fn ifdef_and_ifndef_ask_the_macro_table() {
        assert_eq!(clean("#define F 1\n#ifdef F\nyes\n#endif\n"), "yes");
        assert_eq!(clean("#ifdef F\nyes\n#endif\n"), "");
        assert_eq!(clean("#ifndef F\nyes\n#endif\n"), "yes");
        // C23 spells the two of them as `#elifdef` and `#elifndef` as well.
        assert_eq!(clean("#define F 1\n#if 0\na\n#elifdef F\nb\n#endif\n"), "b");
        assert_eq!(clean("#if 0\na\n#elifndef F\nb\n#endif\n"), "b");
    }

    #[test]
    fn only_the_first_true_branch_of_a_chain_is_taken() {
        assert_eq!(clean("#if 0\na\n#elif 1\nb\n#elif 1\nc\n#else\nd\n#endif\n"), "b");
        assert_eq!(clean("#if 0\na\n#elif 0\nb\n#else\nc\n#endif\n"), "c");
    }

    #[test]
    fn a_branch_after_one_that_was_taken_is_not_evaluated() {
        // `1/0` in a branch that cannot be reached is legal, and headers rely on it: the
        // guard that made the branch dead is often the thing that made the expression safe.
        assert_eq!(clean("#if 1\na\n#elif 1/0\nb\n#endif\n"), "a");
    }

    #[test]
    fn a_skipped_region_is_not_read_for_anything_but_nesting() {
        // Prose, an unknown directive and a broken `#define` all have to pass silently.
        let src = "#if 0\nthis is not C at all\n#frobnicate\n#define\n#if 1\ninner\n#endif\n#endif\nafter\n";
        assert_eq!(clean(src), "after");
    }

    #[test]
    fn nesting_inside_a_dead_branch_stays_balanced() {
        let src = "#if 0\n#ifdef X\na\n#else\nb\n#endif\n#else\nc\n#endif\n";
        assert_eq!(clean(src), "c");
    }

    #[test]
    fn defined_works_in_both_spellings_and_before_expansion() {
        assert_eq!(clean("#define F 0\n#if defined F\nyes\n#endif\n"), "yes");
        assert_eq!(clean("#define F 0\n#if defined(F)\nyes\n#endif\n"), "yes");
        assert_eq!(clean("#if defined(F)\nyes\n#endif\n"), "");
        // `F` expands to 0, but `defined F` is answered before that happens, which is the
        // whole reason `defined` is resolved in a pass of its own.
        assert_eq!(clean("#define F 0\n#if defined F && !F\nyes\n#endif\n"), "yes");
    }

    #[test]
    fn an_identifier_that_survived_expansion_is_zero() {
        assert_eq!(clean("#if NOT_DEFINED_ANYWHERE\nyes\n#else\nno\n#endif\n"), "no");
        assert_eq!(clean("#if !NOT_DEFINED_ANYWHERE\nyes\n#endif\n"), "yes");
    }

    #[test]
    fn short_circuiting_keeps_a_guarded_expression_safe() {
        // The reason `&&` has to short circuit rather than merely produce the right answer:
        // the right hand side divides by zero when the guard is false.
        assert_eq!(clean("#if defined(F) && 1/F\nyes\n#else\nno\n#endif\n"), "no");
        assert_eq!(clean("#if 1 ? 2 : 1/0\nyes\n#endif\n"), "yes");
    }

    #[test]
    fn the_operators_have_the_precedence_they_do_in_c() {
        assert_eq!(clean("#if 1 + 2 * 3 == 7\nyes\n#endif\n"), "yes");
        assert_eq!(clean("#if (1 + 2) * 3 == 9\nyes\n#endif\n"), "yes");
        assert_eq!(clean("#if 1 << 4 == 16\nyes\n#endif\n"), "yes");
        assert_eq!(clean("#if -8 / 3 == -2\nyes\n#endif\n"), "yes");
        assert_eq!(clean("#if (0xff & 0x0f) == 15\nyes\n#endif\n"), "yes");
    }

    #[test]
    fn an_unsigned_operand_makes_the_whole_comparison_unsigned() {
        // The rule that catches everyone out in C catches them out here too, and a
        // preprocessor that quietly disagreed with the compiler would be worse than one that
        // is merely surprising.
        assert_eq!(clean("#if -1 < 0u\nyes\n#else\nno\n#endif\n"), "no");
        assert_eq!(clean("#if -1 < 0\nyes\n#else\nno\n#endif\n"), "yes");
    }

    #[test]
    fn character_constants_evaluate() {
        assert_eq!(clean("#if 'A' == 65\nyes\n#endif\n"), "yes");
        assert_eq!(clean("#if '\\n' == 10\nyes\n#endif\n"), "yes");
    }

    #[test]
    fn a_macro_is_expanded_before_the_expression_is_evaluated() {
        assert_eq!(clean("#define V 3\n#if V > 2\nyes\n#endif\n"), "yes");
        assert_eq!(clean("#define M(a) ((a) * 2)\n#if M(3) == 6\nyes\n#endif\n"), "yes");
    }

    #[test]
    fn an_invocation_may_span_lines_within_a_run_of_text() {
        assert_eq!(clean("#define M(a, b) a + b\nM(1,\n2)\n"), "1 + 2");
    }

    #[test]
    fn undef_removes_a_definition() {
        assert_eq!(clean("#define F 1\n#undef F\n#ifdef F\nyes\n#else\nno\n#endif\n"), "no");
        // Undefining something that was never defined is not an error, and configure scripts
        // emit it constantly.
        assert_eq!(clean("#undef NEVER_DEFINED\nok\n"), "ok");
    }

    #[test]
    fn some_names_cannot_be_undefined() {
        let mut run = Run::new();
        run.go("#undef defined\n");
        assert_eq!(run.messages(), vec!["`defined` cannot be undefined".to_owned()]);
    }

    #[test]
    fn error_reports_the_rest_of_the_line() {
        let mut run = Run::new();
        run.go("#if 0\n#error not this one\n#else\n#error unsupported target\n#endif\n");
        assert_eq!(run.messages(), vec!["unsupported target".to_owned()]);
    }

    #[test]
    fn warning_is_a_warning() {
        let mut run = Run::new();
        run.go("#warning this is fine\n");
        assert_eq!(run.severities(), vec![Severity::Warning]);
        assert_eq!(run.messages(), vec!["this is fine".to_owned()]);
    }

    #[test]
    fn an_unterminated_conditional_is_reported() {
        let mut run = Run::new();
        assert_eq!(run.go("#if 1\nyes\n"), "yes");
        assert_eq!(run.messages(), vec!["unterminated `#if`".to_owned()]);
    }

    #[test]
    fn a_conditional_without_an_if_is_reported() {
        let mut run = Run::new();
        run.go("#endif\n");
        assert_eq!(run.messages(), vec!["`#endif` without `#if`".to_owned()]);

        let mut run = Run::new();
        run.go("#if 1\n#else\n#else\n#endif\n");
        assert_eq!(run.messages(), vec!["a second `#else`".to_owned()]);

        let mut run = Run::new();
        run.go("#if 1\n#else\n#elif 1\n#endif\n");
        assert_eq!(run.messages(), vec!["`#elif` after `#else`".to_owned()]);
    }

    #[test]
    fn tokens_after_endif_are_a_warning_rather_than_an_error() {
        // `#endif FOO` as a hand written comment predates `//` being portable and there is a
        // great deal of it about. Refusing to compile it would be correct and useless.
        let mut run = Run::new();
        assert_eq!(run.go("#if 1\nyes\n#endif FOO\n"), "yes");
        assert_eq!(run.severities(), vec![Severity::Warning]);
        assert_eq!(run.messages(), vec!["extra tokens after `#endif`".to_owned()]);
    }

    #[test]
    fn the_null_directive_does_nothing() {
        assert_eq!(clean("#\na\n#\nb\n"), "a b");
    }

    #[test]
    fn an_unknown_directive_is_an_error_when_the_region_is_live() {
        let mut run = Run::new();
        run.go("#frobnicate\n");
        assert_eq!(run.messages(), vec!["invalid preprocessing directive".to_owned()]);
    }

    #[test]
    fn line_is_recorded_for_the_source_map() {
        let mut run = Run::new();
        run.go("#line 42 \"other.c\"\n");
        assert!(run.messages().is_empty());
        let recorded = run.pp.line_directives();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].line, 42);
        let file = recorded[0].file.expect("a file name was given");
        assert_eq!(run.interner.resolve(file), "\"other.c\"");
    }

    #[test]
    fn a_line_number_out_of_range_is_refused() {
        let mut run = Run::new();
        run.go("#line 0\n");
        assert_eq!(run.messages(), vec!["`#line` number is out of range".to_owned()]);

        let mut run = Run::new();
        run.go("#line notanumber\n");
        assert_eq!(run.messages(), vec!["`#line` needs a decimal line number".to_owned()]);
    }

    #[test]
    fn a_pragma_passes_through_unchanged() {
        assert_eq!(clean("#pragma pack(1)\nint x;\n"), "#pragma pack(1) int x;");
    }

    #[test]
    fn the_pragma_operator_becomes_a_pragma() {
        assert_eq!(
            clean("_Pragma(\"GCC visibility push(default)\")\nint x;\n"),
            "#pragma GCC visibility push(default) int x;"
        );
    }

    #[test]
    fn the_pragma_operator_works_from_inside_a_macro() {
        // This is the entire reason `_Pragma` exists: a `#pragma` cannot be written in a macro
        // body, so a header that wants to wrap one has no other option.
        let src = "#define PUSH _Pragma(\"pack(push)\")\nPUSH\nint x;\n";
        assert_eq!(clean(src), "#pragma pack(push) int x;");
    }

    #[test]
    fn a_pragma_operator_that_is_not_given_a_string_is_reported() {
        let mut run = Run::new();
        run.go("_Pragma(x)\n");
        assert_eq!(run.messages(), vec!["`_Pragma` takes a single string literal".to_owned()]);
    }

    #[test]
    fn an_include_reads_the_file_it_names() {
        let mut run = Run::new();
        run.file("/dir/one.h", "int from_the_header;\n");
        run.dir("/dir");
        assert_eq!(run.go("#include <one.h>\nint after;\n"), "int from_the_header; int after;");
        assert!(run.messages().is_empty());
    }

    #[test]
    fn a_quoted_include_looks_next_to_the_including_file_first() {
        let mut run = Run::new();
        run.file("/local.h", "beside\n");
        run.file("/dir/local.h", "on the path\n");
        run.dir("/dir");
        assert_eq!(run.go("#include \"local.h\"\n"), "beside");
        assert!(run.messages().is_empty());
    }

    #[test]
    fn an_angled_include_does_not_look_next_to_the_including_file() {
        let mut run = Run::new();
        run.file("/local.h", "beside\n");
        run.file("/dir/local.h", "on the path\n");
        run.dir("/dir");
        assert_eq!(run.go("#include <local.h>\n"), "on the path");
    }

    #[test]
    fn a_macro_defined_in_a_header_is_visible_after_the_include() {
        let mut run = Run::new();
        run.file("/dir/defs.h", "#define N 42\n");
        run.dir("/dir");
        assert_eq!(run.go("#include <defs.h>\nint a = N;\n"), "int a = 42;");
        assert!(run.messages().is_empty());
    }

    #[test]
    fn an_include_guard_keeps_the_second_read_empty() {
        let mut run = Run::new();
        run.file("/dir/g.h", "#ifndef G\n#define G\nonce\n#endif\n");
        run.dir("/dir");
        assert_eq!(run.go("#include <g.h>\n#include <g.h>\n"), "once");
        assert!(run.messages().is_empty());
        assert_eq!(run.files(), 2, "the second include is not opened at all");
    }

    #[test]
    fn the_other_spelling_of_a_guard_is_recognised_too() {
        for guard in ["#if !defined(G)", "#if !defined G"] {
            let mut run = Run::new();
            run.file("/dir/g.h", &format!("{guard}\n#define G\nonce\n#endif\n"));
            run.dir("/dir");
            assert_eq!(run.go("#include <g.h>\n#include <g.h>\n"), "once");
            assert_eq!(run.files(), 2, "{guard} should be a guard");
        }
    }

    #[test]
    fn a_conditional_that_is_not_a_guard_does_not_skip_anything() {
        // Nothing defines the macro, so the second read is not the same as the first and the
        // file has to be opened again.
        let mut run = Run::new();
        run.file("/dir/g.h", "#ifndef G\ntwice\n#endif\n");
        run.dir("/dir");
        assert_eq!(run.go("#include <g.h>\n#include <g.h>\n"), "twice twice");
        assert_eq!(run.files(), 3);
    }

    #[test]
    fn a_token_outside_the_guard_stops_it_being_a_guard() {
        let mut run = Run::new();
        run.file("/dir/g.h", "#ifndef G\n#define G\n#endif\nalways\n");
        run.dir("/dir");
        assert_eq!(run.go("#include <g.h>\n#include <g.h>\n"), "always always");
        assert_eq!(run.files(), 3);
    }

    #[test]
    fn pragma_once_skips_the_second_read_and_does_not_reach_the_output() {
        let mut run = Run::new();
        run.file("/dir/o.h", "#pragma once\nonce\n");
        run.dir("/dir");
        assert_eq!(run.go("#include <o.h>\n#include <o.h>\n"), "once");
        assert!(run.messages().is_empty());
        assert_eq!(run.files(), 2);
    }

    #[test]
    fn pragma_once_in_the_main_file_is_a_warning() {
        // It cannot do anything there, and a line that cannot do anything is more likely to
        // be a mistake than a no-op. GCC says the same.
        let mut run = Run::new();
        assert_eq!(run.go("#pragma once\nx\n"), "x");
        assert_eq!(run.severities(), vec![Severity::Warning]);
        assert_eq!(run.messages(), vec!["`#pragma once` in the main file".to_owned()]);
    }

    #[test]
    fn any_other_pragma_still_passes_through() {
        assert_eq!(clean("#pragma once_upon_a_time\n"), "#pragma once_upon_a_time");
    }

    #[test]
    fn has_include_answers_from_the_search_path() {
        let mut run = Run::new();
        run.file("/dir/there.h", "");
        run.dir("/dir");
        let src = "#if __has_include(<there.h>)\nyes\n#endif\n\
                   #if __has_include(<gone.h>)\nno\n#endif\n";
        assert_eq!(run.go(src), "yes");
        assert!(run.messages().is_empty(), "a header that is not there is an answer, not an error");
    }

    #[test]
    fn has_include_asks_the_question_the_include_on_the_same_line_would() {
        // The quoted form looks next to the file that wrote it, so the two spellings answer
        // differently about the same header. A `__has_include` that did not agree with the
        // `#include` it guards would be worse than not having one.
        let mut run = Run::new();
        run.file("/beside.h", "");
        let src = "#if __has_include(\"beside.h\")\nquoted\n#endif\n\
                   #if __has_include(<beside.h>)\nangled\n#endif\n";
        assert_eq!(run.go(src), "quoted");
    }

    #[test]
    fn has_include_next_starts_where_include_next_would() {
        let mut run = Run::new();
        run.file("/a/both.h", "#if __has_include_next(<both.h>)\nmore\n#endif\n");
        run.file("/b/both.h", "last\n");
        run.file("/a/only.h", "#if __has_include_next(<only.h>)\nmore\n#endif\n");
        run.dir("/a");
        run.dir("/b");
        assert_eq!(run.go("#include <both.h>\n"), "more");
        assert_eq!(run.go("#include <only.h>\n"), "", "there is nothing after /a to find it in");
    }

    #[test]
    fn the_operand_of_has_include_is_not_macro_expanded() {
        // `linux` is a predefined macro on a Linux target, and `<linux/version.h>` is a real
        // header. Expanding the operand would ask about `<1/version.h>`.
        let mut run = Run::new();
        run.file("/dir/linux/version.h", "");
        run.dir("/dir");
        let src = "#define linux 1\n#if __has_include(<linux/version.h>)\nyes\n#endif\n";
        assert_eq!(run.go(src), "yes");
    }

    #[test]
    fn a_macro_may_expand_to_a_has_include() {
        // Which is why the operators are resolved after expansion as well as before it.
        let mut run = Run::new();
        run.file("/dir/there.h", "");
        run.dir("/dir");
        let src = "#define HAVE __has_include(<there.h>)\n#if HAVE\nyes\n#endif\n";
        assert_eq!(run.go(src), "yes");
    }

    #[test]
    fn defined_says_the_has_operators_are_there() {
        // The shape every header that uses them is written in, because they are newer than
        // some of the compilers it has to build under.
        let src = "#if defined(__has_include) && defined __has_builtin\nyes\n#endif\n";
        assert_eq!(clean(src), "yes");
        assert_eq!(clean("#ifdef __has_attribute\nyes\n#endif\n"), "yes");
    }

    #[test]
    fn has_attribute_answers_out_of_the_matrix() {
        // No attribute is implemented until the parser lands, and the table saying so is the
        // whole point: a yes here would send a header down a path that then fails to compile.
        assert_eq!(clean("#if __has_attribute(packed)\nyes\n#endif\n"), "");
        assert_eq!(clean("#if __has_attribute(no_such_attribute)\nyes\n#endif\n"), "");
        assert_eq!(clean("#if !__has_attribute(packed)\nno\n#endif\n"), "no");
    }

    #[test]
    fn the_scoped_spelling_of_an_attribute_is_the_same_question() {
        // `[[gnu::packed]]` and `__attribute__((packed))` are one attribute, and
        // `__has_c_attribute` answers with the value the standard gives it rather than with
        // one. Both answer zero today because the table says the attribute is unimplemented.
        assert_eq!(clean("#if __has_c_attribute(gnu::packed)\nyes\n#endif\n"), "");
        assert_eq!(clean("#if __has_c_attribute(deprecated)\nyes\n#endif\n"), "");
    }

    #[test]
    fn has_builtin_answers_no_until_the_builtin_is_real() {
        assert_eq!(clean("#if __has_builtin(__builtin_expect)\nyes\n#endif\n"), "");
        assert_eq!(clean("#if __has_builtin(__builtin_nonesuch)\nyes\n#endif\n"), "");
    }

    #[test]
    fn has_feature_and_has_extension_read_the_same_table() {
        // The preprocessor features are the ones that are real today, so they are the ones
        // that answer yes, and `__has_extension` answers yes wherever `__has_feature` does.
        assert_eq!(clean("#if __has_feature(pragma_once)\nyes\n#endif\n"), "yes");
        assert_eq!(clean("#if __has_extension(pragma_once)\nyes\n#endif\n"), "yes");
        assert_eq!(clean("#if __has_extension(include_next)\nyes\n#endif\n"), "yes");
        assert_eq!(clean("#if __has_feature(include_next)\nyes\n#endif\n"), "");
        assert_eq!(clean("#if __has_feature(statement_expressions)\nyes\n#endif\n"), "");
    }

    #[test]
    fn a_has_operator_without_an_operand_is_reported() {
        let mut run = Run::new();
        run.go("#if __has_include\nyes\n#endif\n");
        assert_eq!(run.messages(), ["expected `(` after `__has_include`"]);
        let mut run = Run::new();
        run.go("#if __has_include(1)\nyes\n#endif\n");
        assert_eq!(run.messages(), ["expected a file name in `<>` or `\"\"`"]);
        let mut run = Run::new();
        run.go("#if __has_attribute(\"packed\")\nyes\n#endif\n");
        assert_eq!(run.messages(), ["expected an identifier as the operand of `__has_attribute`"]);
    }

    #[test]
    fn the_predefined_set_is_visible_to_the_source_file() {
        let mut run = Run::new();
        run.predefine("x86_64-unknown-linux-gnu", &Predef::new());
        let src = "#if defined(__x86_64__) && defined(__linux__) && __SIZEOF_LONG__ == 8\n\
                   yes\n#endif\n";
        assert_eq!(run.go(src), "yes");
        assert!(run.messages().is_empty());
    }

    #[test]
    fn the_predefined_set_follows_the_target_and_not_the_host() {
        let mut run = Run::new();
        run.predefine("aarch64-unknown-linux-gnu", &Predef::new());
        assert_eq!(
            run.go("#ifdef __x86_64__\nno\n#endif\n#ifdef __aarch64__\nyes\n#endif\n"),
            "yes"
        );
    }

    #[test]
    fn a_predefined_macro_expands_where_it_is_used() {
        let mut run = Run::new();
        run.predefine("x86_64-unknown-linux-gnu", &Predef::new());
        assert_eq!(run.go("__SIZE_TYPE__ n;\n"), "long unsigned int n;");
    }

    #[test]
    fn a_command_line_define_is_a_definition_like_any_other() {
        let mut opts = Predef::new();
        opts.defines = vec!["FOO".to_owned(), "BAR=3".to_owned()];
        opts.undefines = vec!["__linux__".to_owned()];
        let mut run = Run::new();
        run.predefine("x86_64-unknown-linux-gnu", &opts);
        let src = "#if FOO && BAR == 3 && !defined(__linux__)\nyes\n#endif\n";
        assert_eq!(run.go(src), "yes");
        assert!(run.messages().is_empty());
    }

    #[test]
    fn the_predefined_set_produces_no_tokens_of_its_own() {
        // It is a file of directives, so the output of the compilation is the source file
        // and nothing else. A stray token here would appear at the top of every `-E` run.
        let mut run = Run::new();
        run.predefine("x86_64-unknown-linux-gnu", &Predef::new());
        assert_eq!(run.go("alone\n"), "alone");
    }

    #[test]
    fn the_predefined_files_are_named_the_way_gcc_names_them() {
        let mut run = Run::new();
        let mut opts = Predef::new();
        opts.defines = vec!["FOO=1".to_owned()];
        run.predefine("x86_64-unknown-linux-gnu", &opts);
        let names: Vec<&str> = run.sources.files().iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["<built-in>", "<command-line>"]);
    }

    #[test]
    fn a_dialect_without_the_gnu_extensions_says_so() {
        let mut opts = Predef::new();
        opts.gnu_extensions = false;
        opts.std = Std::C99;
        let mut run = Run::new();
        run.predefine("x86_64-unknown-linux-gnu", &opts);
        let src = "#if defined(__STRICT_ANSI__) && __STDC_VERSION__ == 199901L && !defined(linux)\n\
                   yes\n#endif\n";
        assert_eq!(run.go(src), "yes");
    }

    #[test]
    fn the_date_and_time_are_the_same_for_the_whole_translation_unit() {
        let mut opts = Predef::new();
        opts.timestamp = Timestamp::from_unix(0);
        let mut run = Run::new();
        run.predefine("x86_64-unknown-linux-gnu", &opts);
        assert_eq!(run.go("__DATE__ __TIME__\n"), "\"Jan  1 1970\" \"00:00:00\"");
    }

    #[test]
    fn a_has_operator_in_a_dead_branch_is_not_asked_about() {
        // The line is not evaluated at all, so a malformed one inside `#if 0` is text.
        assert_eq!(clean("#if 0\n#if __has_include\n#endif\n#endif\nafter\n"), "after");
    }

    #[test]
    fn a_conditional_may_not_span_an_include() {
        // GCC and Clang both refuse this, and the reason is that a header which opens a
        // conditional it does not close leaves the file that included it in a state nothing
        // downstream can reason about.
        let mut run = Run::new();
        run.file("/dir/open.h", "#if 1\n");
        run.dir("/dir");
        run.go("#include <open.h>\nkept\n#endif\n");
        let messages = run.messages();
        assert_eq!(messages.len(), 2);
        assert!(messages[0].contains("unterminated"));
        assert!(messages[1].contains("without"));
    }

    #[test]
    fn include_next_continues_after_the_directory_the_file_came_from() {
        // The wrapper header trick: `/a` has a `limits.h` that pulls in the real one from
        // `/b`, and the two have the same name on purpose.
        let mut run = Run::new();
        run.file("/a/limits.h", "wrapper\n#include_next <limits.h>\n");
        run.file("/b/limits.h", "real\n");
        run.dir("/a");
        run.dir("/b");
        assert_eq!(run.go("#include <limits.h>\n"), "wrapper real");
        assert!(run.messages().is_empty());
    }

    #[test]
    fn a_computed_include_is_expanded_first() {
        let mut run = Run::new();
        run.file("/dir/sub/thing.h", "computed\n");
        run.dir("/dir");
        let src = "#define HEADER <sub/thing.h>\n#include HEADER\n";
        assert_eq!(run.go(src), "computed");
        assert!(run.messages().is_empty());
        // The string literal form goes through the same path and keeps its delimiters.
        let mut run = Run::new();
        run.file("/dir/sub/thing.h", "computed\n");
        run.dir("/dir");
        assert_eq!(run.go("#define H \"sub/thing.h\"\n#include H\n"), "computed");
    }

    #[test]
    fn a_header_that_is_not_there_says_where_it_looked() {
        let mut run = Run::new();
        run.dir("/dir");
        run.go("#include <nope.h>\n");
        let diagnostics = run.pp.take_diagnostics();
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, Some("E0341"));
        assert_eq!(diagnostics[0].message, "`nope.h` file not found");
        assert!(diagnostics[0].children[0].message.contains("/dir"));
    }

    #[test]
    fn an_include_that_is_not_a_header_name_is_reported() {
        let mut run = Run::new();
        run.go("#include 3\n");
        let diagnostics = run.pp.take_diagnostics();
        assert_eq!(diagnostics[0].code, Some("E0343"));
    }

    #[test]
    fn a_header_that_includes_itself_stops() {
        let mut run = Run::new();
        run.file("/dir/loop.h", "#include <loop.h>\n");
        run.dir("/dir");
        run.go("#include <loop.h>\n");
        let diagnostics = run.pp.take_diagnostics();
        assert_eq!(diagnostics.len(), 1, "one complaint, not one per level");
        assert_eq!(diagnostics[0].code, Some("E0342"));
    }

    #[test]
    fn an_include_in_a_dead_branch_is_not_read() {
        let mut run = Run::new();
        assert_eq!(run.go("#if 0\n#include <nothing.h>\n#endif\nafter\n"), "after");
        assert!(run.messages().is_empty(), "a skipped include is not resolved");
    }

    #[test]
    fn embed_is_refused_rather_than_ignored() {
        let mut run = Run::new();
        run.go("#embed <data.bin>\n");
        assert_eq!(run.messages(), vec!["`#embed` is not implemented yet".to_owned()]);
    }

    #[test]
    fn a_directive_may_have_space_before_the_hash_and_after_it() {
        assert_eq!(clean("  #  define F 1\n#ifdef F\nyes\n#endif\n"), "yes");
    }

    #[test]
    fn a_definition_survives_across_a_conditional() {
        assert_eq!(clean("#if 1\n#define F 7\n#endif\nF\n"), "7");
    }

    #[test]
    fn an_empty_if_expression_is_reported() {
        let mut run = Run::new();
        run.go("#if\n#endif\n");
        assert_eq!(run.messages(), vec!["`#if` with no expression".to_owned()]);
    }

    #[test]
    fn the_file_and_the_line_say_where_the_use_is() {
        let mut run = Run::new();
        run.predefine("x86_64-unknown-linux-gnu", &Predef::new());
        assert_eq!(run.go("__FILE__ __LINE__\n__LINE__\n"), "\"/main.c\" 1 2");
        assert!(run.messages().is_empty());
    }

    #[test]
    fn a_macro_that_mentions_the_line_answers_with_the_call() {
        let mut run = Run::new();
        run.predefine("x86_64-unknown-linux-gnu", &Predef::new());
        run.file("/where.h", "#define WHERE __FILE__ __LINE__\n");
        // The point of the whole arrangement. `assert` is this macro, and a version that
        // answered with the header the macro was written in would name a file the user has
        // never opened and a line that means nothing.
        assert_eq!(run.go("#include \"where.h\"\n\n\nWHERE\n"), "\"/main.c\" 4");
        assert!(run.messages().is_empty());
    }

    #[test]
    fn the_file_name_is_the_file_without_the_directories() {
        let mut run = Run::new();
        run.predefine("x86_64-unknown-linux-gnu", &Predef::new());
        assert_eq!(run.go_named("/deep/down/main.c", "__FILE_NAME__\n"), "\"main.c\"");
    }

    #[test]
    fn a_backslash_in_the_name_is_escaped() {
        let mut run = Run::new();
        run.predefine("x86_64-pc-windows-msvc", &Predef::new());
        // The literal has to mean the path, so the separators are escaped. Getting this wrong
        // turns `\src` into an unknown escape and `\a` into a bell character.
        let text = run.go_named("C:\\src\\main.c", "__FILE__ __FILE_NAME__\n");
        assert_eq!(text, "\"C:\\\\src\\\\main.c\" \"main.c\"");
    }

    #[test]
    fn the_base_file_is_the_one_named_on_the_command_line() {
        let mut run = Run::new();
        run.predefine("x86_64-unknown-linux-gnu", &Predef::new());
        run.file("/deep.h", "__FILE__ __BASE_FILE__\n");
        assert_eq!(run.go("#include \"deep.h\"\n"), "\"/deep.h\" \"/main.c\"");
        assert!(run.messages().is_empty());
    }

    #[test]
    fn the_include_level_counts_the_headers_above_it() {
        let mut run = Run::new();
        run.predefine("x86_64-unknown-linux-gnu", &Predef::new());
        run.file("/one.h", "__INCLUDE_LEVEL__\n#include \"two.h\"\n");
        run.file("/two.h", "__INCLUDE_LEVEL__\n");
        assert_eq!(run.go("__INCLUDE_LEVEL__\n#include \"one.h\"\n"), "0 1 2");
        assert!(run.messages().is_empty());
    }

    #[test]
    fn the_counter_is_a_different_number_every_time() {
        let mut run = Run::new();
        run.predefine("x86_64-unknown-linux-gnu", &Predef::new());
        assert_eq!(run.go("__COUNTER__ __COUNTER__ __COUNTER__\n"), "0 1 2");
    }

    #[test]
    fn the_counter_advances_once_per_argument_rather_than_once_per_use() {
        let mut run = Run::new();
        run.predefine("x86_64-unknown-linux-gnu", &Predef::new());
        // An argument is expanded once however many times the body names it, so `TWICE`
        // produces the same number twice. That is what GCC does, and the reason for it is
        // that expanding an argument twice would report anything wrong inside it twice.
        assert_eq!(run.go("#define TWICE(x) x x\nTWICE(__COUNTER__) __COUNTER__\n"), "0 0 1");
    }

    #[test]
    fn the_line_is_a_number_an_if_can_use() {
        let mut run = Run::new();
        run.predefine("x86_64-unknown-linux-gnu", &Predef::new());
        assert_eq!(run.go("#if __LINE__ == 1 && __INCLUDE_LEVEL__ == 0\nyes\n#endif\n"), "yes");
        assert!(run.messages().is_empty());
    }

    #[test]
    fn the_dynamic_macros_are_defined_like_any_others() {
        let mut run = Run::new();
        run.predefine("x86_64-unknown-linux-gnu", &Predef::new());
        let src = "#ifdef __FILE__\nyes\n#endif\n#undef __LINE__\n#ifndef __LINE__\ngone\n#endif\n";
        assert_eq!(run.go(src), "yes gone");
        assert!(run.messages().is_empty(), "`#undef` of a builtin is allowed, as it is in GCC");
    }

    #[test]
    fn redefining_a_dynamic_macro_warns_and_points_at_the_built_in_file() {
        let mut run = Run::new();
        run.predefine("x86_64-unknown-linux-gnu", &Predef::new());
        assert_eq!(run.go("#define __FILE__ \"mine.c\"\n__FILE__\n"), "\"mine.c\"");
        let complaints = run.pp.take_diagnostics();
        assert_eq!(complaints.len(), 1);
        assert_eq!(complaints[0].code, Some("W0301"));
        let previous = complaints[0].children.first().expect("a note saying where it was");
        assert_eq!(run.sources.lookup(previous.span.lo).map(|loc| loc.file), {
            let built_in = run.sources.files().iter().find(|f| f.name == BUILT_IN);
            built_in.map(|f| f.id)
        });
    }

    #[test]
    fn destringizing_undoes_what_stringizing_did() {
        assert_eq!(destringize(r#""a \"b\" c""#), r#"a "b" c"#);
        assert_eq!(destringize(r#""a \\ b""#), r"a \ b");
        assert_eq!(destringize(r#"L"wide""#), "wide");
    }
}
