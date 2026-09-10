//! Reading module definition files, which is where a Windows description comes from.
//!
//! Design: `spec/cross-compile/09-libc-stubs.md` section 9.4, which says a Windows program links
//! against import libraries describing what a DLL exports, that mingw-w64 ships a `.def` file per
//! system DLL, and that turning one into an import library is a mechanical transformation. This
//! module is the reading half of that. It is the direct analogue of [`crate::abilist`] and it is the
//! easier of the two, because there is no versioning anywhere in Windows.
//!
//! A file is a `LIBRARY` statement naming the DLL, an `EXPORTS` statement, and a line per export:
//!
//! ```text
//! LIBRARY "KERNEL32.dll"
//! EXPORTS
//! GetProcAddress@8
//! GetSystemTimeAsFileTime@4
//! ord_103 @103
//! ```
//!
//! The reason to read these rather than a real DLL is the same reason section 9.2 reads glibc's
//! `abilist` files rather than a real `libc.so`. A `.def` is what mingw-w64 checks in and reviews, it
//! is text, and it is the file mingw-w64 itself builds its import libraries from with
//! `dlltool --input-def`.
//!
//! # What the real files use
//!
//! mingw-w64 checks in 2124 of them, under `mingw-w64-crt/lib32`, `lib64`, `libarm64` and
//! `lib-common`, holding 125749 exports between them. Across all of that there are six shapes and no
//! others, counted by running this module over every one of the files:
//!
//! - 124894 are a bare name.
//! - 709 are a name and `DATA`.
//! - 108 are a name, `==` and another name.
//! - 35 are a name and `@` and a number.
//! - 2 are a name, `DATA`, `==` and another name.
//! - 1 is a name, an ordinal and `NONAME`.
//!
//! So `PRIVATE` and `CONSTANT` appear nowhere, and neither does the `=` form, which is the one
//! Microsoft documents and dlltool also accepts. They are read anyway, since a person writing a `.def`
//! by hand is working from Microsoft's page rather than from a census of mingw-w64.
//!
//! 48859 of the names carry an i386 `__stdcall` decoration and 22192 are mangled C++, which is what
//! makes the tokenizing here as conservative as it is: `?GetNextToken@CLexer@@QAEJPAGPAK@Z` is one
//! word, and so is `HeapSize@8`.
//!
//! The `LIBRARY` name comes in three forms and they matter for what an import library records: 622
//! are quoted, as `"ADVAPI32.dll"`, 626 are unquoted with a suffix, and 876 have no dot in them at
//! all, which is every `api-ms-win-*` API set. [`Module::dll`] is where that is settled.
//!
//! # The two traps in the first word
//!
//! Both are real names out of the real files and both cost exports silently, which is why the
//! keyword matching here looks fussier than it needs to be.
//!
//! Four files export `ExportSecurityContext` and four more export `ExportSecurityContext@16`. Both
//! begin with the seven letters of `EXPORTS`, so a reader that matches a statement by prefix eats
//! eight exports out of `secur32` and `sspicli` and says nothing about it.
//!
//! `HeapSize` is an export of `kernel32` and of `api-ms-win-core-heap-l1-1-0`, and `HEAPSIZE` is the
//! statement that sets a heap reservation. So a reader that matches a statement without regard to
//! case loses `HeapSize` out of two files, which is how this module was written first and what reading
//! all 2124 of them found. Every comparison is exact for that reason, which is also what GNU dlltool
//! does: its lexer looks a word up in its keyword table with `strcmp`, and the table holds `HEAPSIZE`
//! and not `HeapSize`. The table holds `DATA` and `data` as two entries, so those two spellings are
//! both read and `Data` is a name, which matters because 31 real lines are mangled C++ names
//! beginning `?Data@`.
//!
//! Inside the export list a keyword in capitals is a name too, which is dlltool's `keyword_as_name`
//! rule, so `HEAPSIZE` after `EXPORTS` is an export. `LIBRARY` is the one keyword dlltool leaves out
//! of that rule, and the comment in its grammar says why: libtool writes the `LIBRARY` statement
//! after `EXPORTS`, which the specification does not allow and which it had to accept anyway.
//!
//! # What it refuses
//!
//! An ordinal of zero, which binutils has a bug report about, because an import at ordinal zero
//! cannot be resolved and the failure is at load time rather than at link time. An ordinal above
//! 65535, since the field an ordinal goes in is two bytes. `NONAME` with no ordinal, which asks for
//! an export with no way at all to name it. `DATA` and `CONSTANT` together, which are two different
//! import types. One name twice in a file, and one ordinal twice, neither of which happens in any of
//! the 2124 real files. Every message carries the line, and the statements this does not implement
//! are refused by name rather than skipped, since a `.def` that sets a load address or a stack size
//! is a `.def` written for a different job.
//!
//! # Where this differs from dlltool
//!
//! Two places, both deliberate.
//!
//! A `LIBRARY` naming a path is refused. dlltool strips the directory and carries on with a warning,
//! and a warning is the right answer when there is somewhere to put one, but there is no warning
//! channel here and silently reading `../lib/foo.dll` as `foo.dll` is the kind of quiet difference
//! section 9.1 is written against.
//!
//! The parts after a name are recognised wherever they appear. Microsoft's grammar puts `=` right
//! after the name and `DATA` at the end, dlltool's puts `==` after `DATA`, and the two disagree about
//! nothing else. Taking them in any order is a superset of both, and every real line uses at most two
//! of them.

use core::fmt;

/// What one module definition file says a DLL exports.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Module {
    /// The name the `LIBRARY` statement gave, with quotes removed and nothing else changed.
    ///
    /// [`Module::dll`] is the name an import library records, which is this one with a suffix when it
    /// has none.
    pub library: String,
    /// Every export, in the order the file listed them.
    ///
    /// File order rather than one of ours, so that a person can read this beside the file line by
    /// line. What is written from it is sorted by whoever writes it.
    pub exports: Vec<Export>,
}

impl Module {
    /// The DLL name an import library records.
    ///
    /// [`Module::library`] with `.dll` appended when it has no dot in it anywhere, which is the rule
    /// both dlltools apply and the reason `api-ms-win-core-apiquery-l2-1-0` becomes a DLL name and
    /// `KERNEL32.dll` is left alone.
    ///
    /// The rule is about a dot rather than about a known suffix, so `windows.ai.machinelearning`,
    /// which mingw-w64 really does check in, comes back unchanged and the DLL it names on disk is
    /// `windows.ai.machinelearning.dll`. That is what GNU dlltool and `llvm-dlltool` both produce
    /// from that file, so it is matched on purpose rather than improved on quietly.
    pub fn dll(&self) -> String {
        if self.library.contains('.') {
            self.library.clone()
        } else {
            format!("{}.dll", self.library)
        }
    }
}

/// One export line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export {
    /// The name the file listed, which is the name a program refers to.
    ///
    /// On i386 this carries the `__stdcall` decoration as mingw-w64 writes it, `GetProcAddress@8`,
    /// and the leading underscore is not here because the file does not have it.
    pub name: String,
    /// The name the DLL's export table carries, from the `==` form, when it differs.
    ///
    /// `getch == _getch` in `api-ms-win-crt-conio-l1-1-0.def` is the shape: a program calls `getch`
    /// and the DLL exports `_getch`.
    pub exported: Option<String>,
    /// The name inside the DLL, from the `=` form, when the file gives one.
    ///
    /// This is for building a DLL rather than for linking against one, so an import library has no
    /// use for it. It is kept because dropping part of a line the file went to the trouble of
    /// writing would be a lie about what was read.
    pub internal: Option<String>,
    /// The ordinal the export is at, when the file gives one.
    pub ordinal: Option<u16>,
    /// Whether the export is code, data or a constant.
    pub form: Form,
    /// Whether the DLL exports it by ordinal only, from `NONAME`.
    pub noname: bool,
    /// Whether it is kept out of the import library, from `PRIVATE`.
    ///
    /// Microsoft's page says `PRIVATE` prevents the name from being included in the import library
    /// LINK generates and does not affect the DLL, so an import library writer drops these. Reading
    /// keeps them, because the file said them.
    pub private: bool,
}

/// What an export is, which decides what an import library has to make of it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Form {
    /// A function, which gets a thunk to jump through as well as the pointer.
    #[default]
    Code,
    /// Data, from `DATA`, which gets a pointer and no thunk.
    ///
    /// Section 9.4 names getting this wrong as the mistake to avoid: a program that reaches data
    /// through a thunk dereferences instructions.
    Data,
    /// A constant, from `CONSTANT`.
    ///
    /// Microsoft calls this obsolete and no mingw-w64 file uses it. It is a third import type rather
    /// than a spelling of one of the other two, so it is read rather than turned into either.
    Constant,
}

/// Reads a module definition file.
///
/// Takes the text rather than a path, since it may come from a checkout, from a sysroot or from a
/// string in a test. mingw-w64 keeps some of these as `.def.in` files with C preprocessor
/// conditionals in them, which its build runs `cpp -P` over first, so what arrives here is the
/// output of that rather than the file with the `#if` in it.
pub fn read(text: &str) -> Result<Module, Error> {
    let mut library: Option<String> = None;
    let mut exporting = false;
    let mut exports: Vec<Export> = Vec::new();
    // Which line each export came from, so that a clash between two of them is reported at the line
    // somebody would go and delete.
    let mut lines: Vec<usize> = Vec::new();

    for (index, text) in text.lines().enumerate() {
        // People and editors count lines from one.
        let line = index + 1;
        let words = words(text);
        let Some(first) = words.first() else { continue };

        match statement(first, exporting) {
            Some(Statement::Library) => {
                if library.is_some() {
                    return Err(Error::Libraries { line });
                }
                library = Some(name_of(&words[1..], line)?);
            }
            Some(Statement::Exports) => {
                exporting = true;
                if words.len() > 1 {
                    // Microsoft's page allows the first definition on the `EXPORTS` line itself.
                    exports.push(export(&words[1..], line)?);
                    lines.push(line);
                }
            }
            Some(Statement::Other(what)) => return Err(Error::Statement { line, what }),
            None => {
                if !exporting {
                    return Err(Error::Loose { line, word: first.to_string() });
                }
                exports.push(export(&words, line)?);
                lines.push(line);
            }
        }
    }

    let Some(library) = library else { return Err(Error::NoLibrary) };
    clashes(&exports, &lines)?;
    Ok(Module { library, exports })
}

/// The statements this module knows about.
enum Statement {
    /// `LIBRARY`, which names the DLL.
    Library,
    /// `EXPORTS`, after which every line is an export.
    Exports,
    /// A statement that is real and is about building an image rather than linking against one.
    Other(&'static str),
}

/// Which statement a word is, matched whole and in capitals, and in the export list only two of them
/// are statements at all.
///
/// Whole is the point twice over. `ExportSecurityContext` is a real export in four of mingw-w64's
/// files and it begins with the letters of `EXPORTS`, so a prefix match eats it. And `HeapSize` is a
/// real export in two more, so a match that ignores case eats that one, which is why every
/// comparison here is exact. dlltool's lexer does the same thing by comparing a word against its
/// keyword table with `strcmp`, and the table holds `HEAPSIZE` and not `HeapSize`.
///
/// Inside the export list, `HEAPSIZE` in capitals is an export name too, which is dlltool's
/// `keyword_as_name` rule. `LIBRARY` is the one keyword it leaves out of that rule, because libtool
/// puts the `LIBRARY` statement after `EXPORTS` and something had to give.
fn statement(word: &str, exporting: bool) -> Option<Statement> {
    if word == "LIBRARY" {
        return Some(Statement::Library);
    }
    if word == "EXPORTS" {
        return Some(Statement::Exports);
    }
    if exporting {
        return None;
    }
    for what in
        ["NAME", "DESCRIPTION", "VERSION", "SECTIONS", "SEGMENTS", "STACKSIZE", "HEAPSIZE", "STUB"]
    {
        if word == what {
            return Some(Statement::Other(what));
        }
    }
    None
}

/// The words on a line, with the comment and the punctuation split off.
///
/// A `;` starts a comment that runs to the end of the line, and `=` and `==` are words of their own
/// however they are spaced, because `getch == _getch` and `func2=func1` are the same two forms with
/// different spacing. Nothing else in a name is punctuation: a C++ name like
/// `??0CLexer@@QAE@XZ` is one word and mingw-w64 has thousands of them.
fn words(line: &str) -> Vec<&str> {
    let line = match line.split_once(';') {
        Some((before, _)) => before,
        None => line,
    };
    let bytes = line.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at].is_ascii_whitespace() {
            at += 1;
        } else if bytes[at] == b'=' {
            let end = if bytes.get(at + 1) == Some(&b'=') { at + 2 } else { at + 1 };
            out.push(&line[at..end]);
            at = end;
        } else {
            let start = at;
            while at < bytes.len() && !bytes[at].is_ascii_whitespace() && bytes[at] != b'=' {
                at += 1;
            }
            out.push(&line[start..at]);
        }
    }
    out
}

/// The name a `LIBRARY` statement gives, with its quotes taken off.
fn name_of(rest: &[&str], line: usize) -> Result<String, Error> {
    let [name] = rest else {
        return Err(Error::Library { line, found: rest.len() });
    };
    let name = match name.strip_prefix('"') {
        Some(inside) => inside.strip_suffix('"').ok_or(Error::Quote { line })?,
        None => name,
    };
    if name.is_empty() {
        return Err(Error::Library { line, found: 0 });
    }
    if name.contains('/') || name.contains('\\') {
        return Err(Error::Path { line, name: name.to_owned() });
    }
    Ok(name.to_owned())
}

/// One export definition, which is a name and then any of the parts that can follow one.
fn export(words: &[&str], line: usize) -> Result<Export, Error> {
    let (name, rest) = words.split_first().expect("a line with no words is skipped before here");
    if let Some(ordinal) = ordinal(name) {
        // A line that is only an ordinal. Microsoft's grammar requires the name and dlltool's does
        // too, and an import library has nothing to call the symbol it would define.
        let _ = ordinal;
        return Err(Error::Nameless { line });
    }

    let mut export = Export {
        name: (*name).to_owned(),
        exported: None,
        internal: None,
        ordinal: None,
        form: Form::Code,
        noname: false,
        private: false,
    };
    let mut form: Option<Form> = None;
    let mut at = 0;
    while at < rest.len() {
        let word = rest[at];
        at += 1;

        if word == "=" || word == "==" {
            let Some(following) = rest.get(at) else {
                return Err(Error::Rename { line, form: word.to_owned() });
            };
            at += 1;
            if ordinal(following).is_some() || *following == "=" || *following == "==" {
                return Err(Error::Rename { line, form: word.to_owned() });
            }
            let slot = if word == "=" { &mut export.internal } else { &mut export.exported };
            if slot.is_some() {
                return Err(Error::Twice { line, part: if word == "=" { "=" } else { "==" } });
            }
            *slot = Some((*following).to_owned());
        } else if let Some(number) = ordinal(word) {
            if export.ordinal.is_some() {
                return Err(Error::Twice { line, part: "an ordinal" });
            }
            match u16::try_from(number) {
                // binutils has a bug report about this one. An import at ordinal zero has nothing to
                // resolve against, and what happens is a program that links and then fails to start.
                Ok(0) => return Err(Error::ZeroOrdinal { line, name: export.name.clone() }),
                Ok(number) => export.ordinal = Some(number),
                Err(_) => return Err(Error::Ordinal { line, ordinal: number }),
            }
        } else if attribute(word, "NONAME", "noname") {
            if export.noname {
                return Err(Error::Twice { line, part: "NONAME" });
            }
            export.noname = true;
        } else if attribute(word, "PRIVATE", "private") {
            if export.private {
                return Err(Error::Twice { line, part: "PRIVATE" });
            }
            export.private = true;
        } else if attribute(word, "DATA", "data") || attribute(word, "CONSTANT", "constant") {
            let found = if attribute(word, "DATA", "data") { Form::Data } else { Form::Constant };
            match form {
                Some(already) if already == found => {
                    return Err(Error::Twice { line, part: "DATA" });
                }
                Some(_) => return Err(Error::Forms { line, name: export.name.clone() }),
                None => form = Some(found),
            }
        } else {
            return Err(Error::Word { line, word: word.to_owned() });
        }
    }

    export.form = form.unwrap_or(Form::Code);
    if export.noname && export.ordinal.is_none() {
        // `NONAME` says the DLL exports it by ordinal only, so without an ordinal there is no way at
        // all to reach it.
        return Err(Error::NoName { line, name: export.name });
    }
    Ok(export)
}

/// One of the words that can follow a name, in either of the two spellings a `.def` may use.
///
/// Capitals or lower case and nothing in between, which reads like an oversight and is not one.
/// dlltool's keyword table holds `DATA` and `data` as two separate entries and compares a word
/// against it with `strcmp`, so `Data` is a name there, and a mangled C++ name beginning `?Data@` is
/// a name in 31 of mingw-w64's real lines. Reading `Data` as an attribute would turn one of those
/// into an export with a word after it.
fn attribute(word: &str, upper: &str, lower: &str) -> bool {
    word == upper || word == lower
}

/// A word that is `@` and a decimal number, which is how an ordinal is written.
///
/// A name may hold an `@` too, since that is what a `__stdcall` decoration is, so the test is that
/// the whole of the rest is digits: `@103` is an ordinal and `GetProcAddress@8` is a name.
fn ordinal(word: &str) -> Option<u64> {
    let digits = word.strip_prefix('@')?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    // A number too long to be a `u64` is an ordinal that is far too large either way, so it is
    // reported as one rather than as an unrecognised word.
    Some(digits.parse().unwrap_or(u64::MAX))
}

/// Refuses one name twice and one ordinal twice.
///
/// By sorting indices rather than by a hash map, because this crate has none: what a stub contains
/// has to be a function of the description and not of an iteration order.
fn clashes(exports: &[Export], lines: &[usize]) -> Result<(), Error> {
    let mut order: Vec<usize> = (0..exports.len()).collect();
    order.sort_by(|&a, &b| exports[a].name.cmp(&exports[b].name).then(lines[a].cmp(&lines[b])));
    for pair in order.windows(2) {
        let (first, second) = (pair[0], pair[1]);
        if exports[first].name == exports[second].name {
            return Err(Error::Repeated {
                line: lines[second],
                first: lines[first],
                name: exports[second].name.clone(),
            });
        }
    }

    let mut numbered: Vec<usize> =
        (0..exports.len()).filter(|&at| exports[at].ordinal.is_some()).collect();
    numbered.sort_by(|&a, &b| {
        exports[a].ordinal.cmp(&exports[b].ordinal).then(lines[a].cmp(&lines[b]))
    });
    for pair in numbered.windows(2) {
        let (first, second) = (pair[0], pair[1]);
        if exports[first].ordinal == exports[second].ordinal {
            return Err(Error::Ordinals {
                line: lines[second],
                first: lines[first],
                ordinal: exports[second].ordinal.expect("only numbered exports are here"),
            });
        }
    }
    Ok(())
}

/// Why a module definition file could not be read.
///
/// Every one of these is the file not being what this module reads, and all but one carry the line,
/// so a message says where to look as well as what is wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// There is no `LIBRARY` statement, so nothing says which DLL this is.
    NoLibrary,
    /// There is more than one `LIBRARY` statement.
    Libraries {
        /// The line the second one is on, counting from one.
        line: usize,
    },
    /// A `LIBRARY` statement is not one name.
    Library {
        /// The line, counting from one.
        line: usize,
        /// How many words followed the statement.
        found: usize,
    },
    /// A quoted `LIBRARY` name has no closing quote.
    Quote {
        /// The line, counting from one.
        line: usize,
    },
    /// A `LIBRARY` names a path rather than a DLL.
    Path {
        /// The line, counting from one.
        line: usize,
        /// The name as written.
        name: String,
    },
    /// A statement that is real and is about building an image rather than linking against one.
    Statement {
        /// The line, counting from one.
        line: usize,
        /// The statement.
        what: &'static str,
    },
    /// A line before `EXPORTS` that is not a statement this module knows.
    Loose {
        /// The line, counting from one.
        line: usize,
        /// The first word on it.
        word: String,
    },
    /// An export line begins with an ordinal, so there is no name to define.
    Nameless {
        /// The line, counting from one.
        line: usize,
    },
    /// A word after the name is not a part of an export definition.
    Word {
        /// The line, counting from one.
        line: usize,
        /// The word.
        word: String,
    },
    /// One part of an export definition is given twice.
    Twice {
        /// The line, counting from one.
        line: usize,
        /// The part.
        part: &'static str,
    },
    /// A `=` or `==` has no name after it.
    Rename {
        /// The line, counting from one.
        line: usize,
        /// Which of the two it was.
        form: String,
    },
    /// An export is both `DATA` and `CONSTANT`, which are different import types.
    Forms {
        /// The line, counting from one.
        line: usize,
        /// The name.
        name: String,
    },
    /// An ordinal is zero, which is not an ordinal any import can resolve against.
    ZeroOrdinal {
        /// The line, counting from one.
        line: usize,
        /// The name.
        name: String,
    },
    /// An ordinal does not fit in the two bytes an ordinal is stored in.
    Ordinal {
        /// The line, counting from one.
        line: usize,
        /// The ordinal as read.
        ordinal: u64,
    },
    /// `NONAME` with no ordinal, so the export has no name and no number either.
    NoName {
        /// The line, counting from one.
        line: usize,
        /// The name.
        name: String,
    },
    /// One name is exported twice.
    Repeated {
        /// The line the second one is on, counting from one.
        line: usize,
        /// The line the first one is on.
        first: usize,
        /// The name.
        name: String,
    },
    /// One ordinal is used twice.
    Ordinals {
        /// The line the second one is on, counting from one.
        line: usize,
        /// The line the first one is on.
        first: usize,
        /// The ordinal.
        ordinal: u16,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NoLibrary => write!(
                f,
                "there is no LIBRARY statement, so nothing says which DLL these exports are in"
            ),
            Error::Libraries { line } => {
                write!(f, "line {line}: a second LIBRARY statement, and a file describes one DLL")
            }
            Error::Library { line, found } => {
                write!(f, "line {line}: LIBRARY takes one name and this one has {found} words")
            }
            Error::Quote { line } => {
                write!(f, "line {line}: the LIBRARY name opens a quote and does not close it")
            }
            Error::Path { line, name } => write!(
                f,
                "line {line}: the LIBRARY name `{name}` is a path, and what goes in an import \
                 library is the name the loader looks for"
            ),
            Error::Statement { line, what } => write!(
                f,
                "line {line}: {what} is about building an image rather than linking against one, \
                 and nothing here reads it"
            ),
            Error::Loose { line, word } => {
                write!(f, "line {line}: `{word}` comes before any EXPORTS statement")
            }
            Error::Nameless { line } => write!(
                f,
                "line {line}: the line begins with an ordinal, so there is no name to export"
            ),
            Error::Word { line, word } => write!(
                f,
                "line {line}: `{word}` is not part of an export, which after the name is =name, \
                 ==name, @ordinal, NONAME, PRIVATE, DATA or CONSTANT"
            ),
            Error::Twice { line, part } => write!(
                f,
                "line {line}: {part} is given twice on one export, and the second one cannot say \
                 anything the first did not"
            ),
            Error::Rename { line, form } => {
                write!(f, "line {line}: `{form}` has no name after it")
            }
            Error::Forms { line, name } => write!(
                f,
                "line {line}: `{name}` is both DATA and CONSTANT, which are different kinds of \
                 import"
            ),
            Error::ZeroOrdinal { line, name } => write!(
                f,
                "line {line}: `{name}` is at ordinal 0, which no import can resolve against"
            ),
            Error::Ordinal { line, ordinal } => {
                write!(f, "line {line}: {ordinal} does not fit in the two bytes an ordinal has")
            }
            Error::NoName { line, name } => write!(
                f,
                "line {line}: `{name}` is NONAME with no ordinal, so nothing can reach it at all"
            ),
            Error::Repeated { line, first, name } => {
                write!(f, "line {line}: `{name}` is already exported on line {first}")
            }
            Error::Ordinals { line, first, ordinal } => {
                write!(f, "line {line}: ordinal {ordinal} is already used on line {first}")
            }
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape mingw-w64 checks in, with one of each of the six real forms.
    const SOME: &str = "\
LIBRARY \"KERNEL32.dll\"
EXPORTS
GetProcAddress@8
DnsGlobals DATA
getch == _getch
ord_103 @103
__msvcrt_iswctype DATA == iswctype
SaferiRegisterExtensionDll@8 @1000 NONAME
";

    #[test]
    fn the_six_shapes_the_real_files_use_all_read() {
        let module = read(SOME).unwrap();
        assert_eq!(module.library, "KERNEL32.dll");
        assert_eq!(module.exports.len(), 6);

        let by = |name: &str| module.exports.iter().find(|e| e.name == name).unwrap().clone();
        assert_eq!(by("GetProcAddress@8").form, Form::Code);
        assert_eq!(by("GetProcAddress@8").ordinal, None);
        assert_eq!(by("DnsGlobals").form, Form::Data);
        assert_eq!(by("getch").exported.as_deref(), Some("_getch"));
        assert_eq!(by("ord_103").ordinal, Some(103));
        assert_eq!(by("__msvcrt_iswctype").form, Form::Data);
        assert_eq!(by("__msvcrt_iswctype").exported.as_deref(), Some("iswctype"));
        assert_eq!(by("SaferiRegisterExtensionDll@8").ordinal, Some(1000));
        assert!(by("SaferiRegisterExtensionDll@8").noname);
    }

    #[test]
    fn a_name_that_starts_with_the_letters_of_a_statement_is_an_export() {
        // The real one. secur32 and sspicli export both of these, and a reader that matched EXPORTS
        // by prefix would drop eight exports across four files without a word.
        let module =
            read("LIBRARY secur32.dll\nEXPORTS\nExportSecurityContext\nExportSecurityContext@16\n")
                .unwrap();
        let names: Vec<&str> = module.exports.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["ExportSecurityContext", "ExportSecurityContext@16"]);
    }

    #[test]
    fn a_name_that_is_a_statement_in_another_case_is_an_export() {
        // The other real one. kernel32 and api-ms-win-core-heap-l1-1-0 both export HeapSize, and
        // HEAPSIZE is the statement that reserves a heap, so matching a statement without regard to
        // case drops a real export out of two files.
        let module =
            read("LIBRARY kernel32.dll\nEXPORTS\nHeapAlloc\nHeapSize\nHeapFree\n").unwrap();
        let names: Vec<&str> = module.exports.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["HeapAlloc", "HeapSize", "HeapFree"]);
    }

    #[test]
    fn a_keyword_in_the_export_list_is_a_name_and_library_is_not() {
        // dlltool's keyword_as_name rule, and the one exception its grammar carves out of it with a
        // comment about libtool.
        let module = read("LIBRARY k.dll\nEXPORTS\nHEAPSIZE\nDATA\n?Data@@XZ\n").unwrap();
        let names: Vec<&str> = module.exports.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["HEAPSIZE", "DATA", "?Data@@XZ"]);

        // The libtool shape, which puts the statement after the list.
        let after = read("EXPORTS\nf\nLIBRARY k.dll\n").unwrap();
        assert_eq!(after.library, "k.dll");
        assert_eq!(after.exports.len(), 1);
    }

    #[test]
    fn an_attribute_is_read_in_capitals_or_in_lower_case_and_in_nothing_between() {
        assert_eq!(read("LIBRARY k.dll\nEXPORTS\nf DATA\n").unwrap().exports[0].form, Form::Data);
        assert_eq!(read("LIBRARY k.dll\nEXPORTS\nf data\n").unwrap().exports[0].form, Form::Data);
        // Which dlltool would read as a second word on the line rather than as an attribute, because
        // `Data` is not in its keyword table in that spelling.
        assert_eq!(
            read("LIBRARY k.dll\nEXPORTS\nf Data\n"),
            Err(Error::Word { line: 3, word: "Data".to_owned() })
        );
    }

    #[test]
    fn a_decorated_name_is_a_name_and_an_ordinal_is_not() {
        // The `@` in `GetProcAddress@8` is the stdcall decoration and the `@103` on its own is an
        // ordinal, so the difference is whether anything comes before the `@`.
        let module = read("LIBRARY k.dll\nEXPORTS\nGetProcAddress@8 @103\n").unwrap();
        assert_eq!(module.exports[0].name, "GetProcAddress@8");
        assert_eq!(module.exports[0].ordinal, Some(103));
    }

    #[test]
    fn a_mangled_cpp_name_is_one_word() {
        // ks.def has thousands of these. They hold `?`, `@` and `$` and none of that is punctuation
        // here.
        let module = read(
            "LIBRARY ks.dll\nEXPORTS\n??0CLexer@@QAE@XZ\n?GetNextToken@CLexer@@QAEJPAGPAK@Z\n",
        )
        .unwrap();
        assert_eq!(module.exports.len(), 2);
        assert_eq!(module.exports[0].name, "??0CLexer@@QAE@XZ");
    }

    #[test]
    fn comments_and_blank_lines_are_not_a_problem() {
        let text = "\
; This file is a comprehensive documentation for 32-bit x86 advapi32.dll symbols.
LIBRARY \"ADVAPI32.dll\"
EXPORTS

GetMangledSiteSid@12 ; removed in Windows XP
TraceMessage ; cdecl
";
        let module = read(text).unwrap();
        assert_eq!(module.exports.len(), 2);
        assert_eq!(module.exports[1].name, "TraceMessage");
    }

    #[test]
    fn the_dll_name_gets_a_suffix_only_when_it_has_no_dot_at_all() {
        let dll = |library: &str| read(&format!("LIBRARY {library}\nEXPORTS\nf\n")).unwrap().dll();
        assert_eq!(dll("KERNEL32.dll"), "KERNEL32.dll");
        assert_eq!(dll("\"ADVAPI32.dll\""), "ADVAPI32.dll");
        assert_eq!(dll("api-ms-win-core-apiquery-l2-1-0"), "api-ms-win-core-apiquery-l2-1-0.dll");
        assert_eq!(dll("ntoskrnl.exe"), "ntoskrnl.exe");
        // The one real file the rule reads oddly. Both dlltools do the same thing with it, which is
        // why this is a test of what happens rather than a note about what should.
        assert_eq!(dll("windows.ai.machinelearning"), "windows.ai.machinelearning");
    }

    #[test]
    fn the_spacing_around_a_rename_does_not_matter() {
        let spaced = read("LIBRARY k.dll\nEXPORTS\nfunc2 = func1\n").unwrap();
        let tight = read("LIBRARY k.dll\nEXPORTS\nfunc2=func1\n").unwrap();
        assert_eq!(spaced.exports, tight.exports);
        assert_eq!(spaced.exports[0].internal.as_deref(), Some("func1"));
        assert_eq!(spaced.exports[0].exported, None);
    }

    #[test]
    fn the_parts_after_a_name_are_taken_in_any_order() {
        // Microsoft puts `=` first and `DATA` last, dlltool puts `==` after `DATA`, and taking them
        // in any order agrees with both.
        let one = read("LIBRARY k.dll\nEXPORTS\nf DATA @7 == g\n").unwrap();
        let two = read("LIBRARY k.dll\nEXPORTS\nf == g @7 DATA\n").unwrap();
        assert_eq!(one.exports, two.exports);
        assert_eq!(one.exports[0].form, Form::Data);
        assert_eq!(one.exports[0].ordinal, Some(7));
        assert_eq!(one.exports[0].exported.as_deref(), Some("g"));
    }

    #[test]
    fn a_definition_may_sit_on_the_exports_line() {
        let module = read("LIBRARY k.dll\nEXPORTS f DATA\ng\n").unwrap();
        assert_eq!(module.exports.len(), 2);
        assert_eq!(module.exports[0].form, Form::Data);
    }

    #[test]
    fn an_ordinal_has_to_be_one_an_import_can_use() {
        let with = |line: &str| read(&format!("LIBRARY k.dll\nEXPORTS\n{line}\n")).unwrap_err();
        assert_eq!(with("f @0"), Error::ZeroOrdinal { line: 3, name: "f".to_owned() });
        assert_eq!(with("f @65536"), Error::Ordinal { line: 3, ordinal: 65536 });
        assert_eq!(with("f NONAME"), Error::NoName { line: 3, name: "f".to_owned() });
        assert_eq!(with("@42"), Error::Nameless { line: 3 });
        assert!(read("LIBRARY k.dll\nEXPORTS\nf @65535\n").is_ok());
    }

    #[test]
    fn one_name_twice_and_one_ordinal_twice_are_both_refused() {
        // Neither happens in any of the 2124 real files, and both would put two answers in an import
        // library for one question.
        assert_eq!(
            read("LIBRARY k.dll\nEXPORTS\nf\ng\nf\n"),
            Err(Error::Repeated { line: 5, first: 3, name: "f".to_owned() })
        );
        assert_eq!(
            read("LIBRARY k.dll\nEXPORTS\nf @7\ng @7\n"),
            Err(Error::Ordinals { line: 4, first: 3, ordinal: 7 })
        );
    }

    #[test]
    fn the_statements_this_does_not_read_are_refused_by_name() {
        assert_eq!(
            read("LIBRARY k.dll\nSTACKSIZE 4096\nEXPORTS\nf\n"),
            Err(Error::Statement { line: 2, what: "STACKSIZE" })
        );
        assert_eq!(
            read("NAME program.exe\nEXPORTS\nf\n"),
            Err(Error::Statement { line: 1, what: "NAME" })
        );
    }

    #[test]
    fn a_file_has_to_say_which_dll_it_is_and_say_it_once() {
        assert_eq!(read("EXPORTS\nf\n"), Err(Error::NoLibrary));
        assert_eq!(
            read("LIBRARY a.dll\nLIBRARY b.dll\nEXPORTS\nf\n"),
            Err(Error::Libraries { line: 2 })
        );
        assert_eq!(read("LIBRARY\nEXPORTS\nf\n"), Err(Error::Library { line: 1, found: 0 }));
        // `BASE=` is a real option on Microsoft's LIBRARY statement and it sets a load address, which
        // is a thing about building a DLL. Four words rather than three, since `=` is punctuation.
        assert_eq!(
            read("LIBRARY a.dll BASE=0x1000\nEXPORTS\nf\n"),
            Err(Error::Library { line: 1, found: 4 })
        );
        assert_eq!(read("LIBRARY \"a.dll\nEXPORTS\nf\n"), Err(Error::Quote { line: 1 }));
        assert_eq!(
            read("LIBRARY ../lib/a.dll\nEXPORTS\nf\n"),
            Err(Error::Path { line: 1, name: "../lib/a.dll".to_owned() })
        );
    }

    #[test]
    fn an_export_before_exports_is_refused_rather_than_taken() {
        // A file whose EXPORTS is missing would otherwise read as a file with no exports, which is a
        // valid import library that resolves nothing.
        assert_eq!(
            read("LIBRARY k.dll\nGetProcAddress@8\n"),
            Err(Error::Loose { line: 2, word: "GetProcAddress@8".to_owned() })
        );
    }

    #[test]
    fn a_line_that_says_two_things_about_one_export_is_refused() {
        let with = |line: &str| read(&format!("LIBRARY k.dll\nEXPORTS\n{line}\n")).unwrap_err();
        assert_eq!(with("f DATA CONSTANT"), Error::Forms { line: 3, name: "f".to_owned() });
        assert_eq!(with("f DATA DATA"), Error::Twice { line: 3, part: "DATA" });
        assert_eq!(with("f @1 @2"), Error::Twice { line: 3, part: "an ordinal" });
        assert_eq!(with("f == g == h"), Error::Twice { line: 3, part: "==" });
        assert_eq!(with("f =="), Error::Rename { line: 3, form: "==".to_owned() });
        assert_eq!(with("f == @2"), Error::Rename { line: 3, form: "==".to_owned() });
        assert_eq!(with("f WHATEVER"), Error::Word { line: 3, word: "WHATEVER".to_owned() });
    }

    #[test]
    fn private_and_constant_are_read_although_no_real_file_uses_them() {
        let module =
            read("LIBRARY k.dll\nEXPORTS\nDllCanUnloadNow @1 PRIVATE\nlimit CONSTANT\n").unwrap();
        assert!(module.exports[0].private);
        assert_eq!(module.exports[1].form, Form::Constant);
    }

    #[test]
    fn every_error_says_something_a_person_can_act_on() {
        let messages = [
            Error::NoLibrary,
            Error::Libraries { line: 2 },
            Error::Library { line: 1, found: 3 },
            Error::Quote { line: 1 },
            Error::Path { line: 1, name: "../a.dll".to_owned() },
            Error::Statement { line: 1, what: "NAME" },
            Error::Loose { line: 2, word: "f".to_owned() },
            Error::Nameless { line: 3 },
            Error::Word { line: 3, word: "WHATEVER".to_owned() },
            Error::Twice { line: 3, part: "DATA" },
            Error::Rename { line: 3, form: "==".to_owned() },
            Error::Forms { line: 3, name: "f".to_owned() },
            Error::ZeroOrdinal { line: 3, name: "f".to_owned() },
            Error::Ordinal { line: 3, ordinal: 65536 },
            Error::NoName { line: 3, name: "f".to_owned() },
            Error::Repeated { line: 5, first: 3, name: "f".to_owned() },
            Error::Ordinals { line: 4, first: 3, ordinal: 7 },
        ];
        for error in messages {
            let said = error.to_string();
            assert!(said.len() > 30, "`{said}` is too short to tell anybody anything");
            assert!(said.chars().next().unwrap().is_lowercase() || said.starts_with("line"));
        }
    }
}
