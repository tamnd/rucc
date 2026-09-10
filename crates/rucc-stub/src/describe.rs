//! What a shared library is, at link time.
//!
//! Design: `spec/cross-compile/09-libc-stubs.md` section 9.1.

use core::fmt;

/// Whether a name belongs to code or to storage.
///
/// The linker chooses a different relocation form for each, so getting this wrong produces PLT
/// and GOT confusion rather than a message about a type, which is why
/// `spec/cross-compile/09-libc-stubs.md` section 9.1 has it in the table of things a stub must get
/// exactly right.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// A function. `STT_FUNC`.
    Function,
    /// Storage, which has a size the linker needs. `STT_OBJECT`.
    Object,
}

/// Whether a link can do without a symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Binding {
    /// Ordinary. `STB_GLOBAL`.
    Global,
    /// Optional, so a reference to it that finds nothing is zero rather than an error.
    /// `STB_WEAK`.
    ///
    /// Describing a weak symbol as global is the last row of section 9.1's table: it turns an
    /// optional symbol into a mandatory one, and the program that notices is the one built against
    /// a libc that happens not to have it.
    Weak,
}

/// One exported name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Symbol {
    /// The name, as the linker will look it up.
    pub name: String,
    /// Code or storage.
    pub kind: Kind,
    /// Whether a link can do without it.
    pub binding: Binding,
    /// How many bytes of storage, for a [`Kind::Object`], and zero for a function.
    ///
    /// This is the other silent row of section 9.1's table. A copy relocation against an object
    /// symbol copies `size` bytes out of the real library into the program's own storage, so a
    /// size that is too small corrupts whatever follows it in a program that linked without a
    /// word of complaint.
    pub size: u64,
}

impl Symbol {
    /// A global function.
    pub fn function(name: impl Into<String>) -> Self {
        Symbol { name: name.into(), kind: Kind::Function, binding: Binding::Global, size: 0 }
    }

    /// A global object of a given size.
    pub fn object(name: impl Into<String>, size: u64) -> Self {
        Symbol { name: name.into(), kind: Kind::Object, binding: Binding::Global, size }
    }

    /// The same symbol, weak.
    #[must_use]
    pub fn weak(mut self) -> Self {
        self.binding = Binding::Weak;
        self
    }
}

/// A library's exported interface, which is everything a linker reads out of one.
///
/// The order symbols are added in does not reach the bytes. [`crate::write`] sorts them, because
/// the caller that will eventually fill this in is a decoder reading a compressed blob and the
/// order it happens to produce is not something claim 5 of `spec/cross-compile/02-the-goal.md`
/// should depend on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Library {
    /// What the program records as the library it needs, and what the loader will go looking for.
    ///
    /// Wrong here means the wrong library is loaded or none is, which is section 9.1's `SONAME`
    /// row.
    pub soname: String,
    /// The libraries this one needs, which become its `DT_NEEDED` chain.
    pub needed: Vec<String>,
    /// Every name the library exports.
    pub symbols: Vec<Symbol>,
}

impl Library {
    /// An empty library with a `SONAME`.
    ///
    /// Empty is a real case rather than a starting point, and section 9.9 is why: build systems pass
    /// `-lpthread` whether or not there is anything behind it, and a missing file is a link error. On
    /// glibc 2.34 and later `libpthread`, `libdl`, `libutil` and `libanl` are inside `libc.so.6` and
    /// the separate files are kept as compatibility stubs that export nothing. `libm` is not one of
    /// them, so which names get an empty library is a question per libc rather than a guess, and
    /// [`compat()`](crate::compat()) is where the answer lives.
    pub fn new(soname: impl Into<String>) -> Self {
        Library { soname: soname.into(), needed: Vec::new(), symbols: Vec::new() }
    }

    /// Adds a library this one needs, at the end of the chain.
    pub fn needs(&mut self, soname: impl Into<String>) -> &mut Self {
        self.needed.push(soname.into());
        self
    }

    /// Adds a symbol.
    pub fn export(&mut self, symbol: Symbol) -> &mut Self {
        self.symbols.push(symbol);
        self
    }

    /// Adds a global function, which is the common case by a wide margin.
    pub fn function(&mut self, name: impl Into<String>) -> &mut Self {
        self.export(Symbol::function(name))
    }

    /// Adds a global object of a given size.
    pub fn object(&mut self, name: impl Into<String>, size: u64) -> &mut Self {
        self.export(Symbol::object(name, size))
    }
}

/// Why a description could not be turned into a stub.
///
/// Every one of these is the description being wrong rather than the writer failing, so they are
/// reported before a byte is produced and none of them can leave a half written file behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The target's object format is not ELF, so this is the wrong writer for it.
    ///
    /// Darwin is here and is not a gap. Apple ships `.tbd` files, which are section 9.1's
    /// technique adopted by the platform vendor, so per section 9.7 they are consumed from the SDK
    /// rather than generated. Windows wants import libraries, which are section 9.4 and a
    /// different container.
    NotElf {
        /// The target that was asked for.
        target: String,
        /// What it writes instead.
        format: &'static str,
    },
    /// The architecture is ELF and what belongs in `e_flags` for it is not settled here.
    ///
    /// The linker refuses to mix objects whose `e_flags` disagree, so a zero guessed for an
    /// architecture that uses the field produces a stub that fails to link for a reason that
    /// points nowhere near the stub. Answering nothing is the better failure.
    NoMachineFlags {
        /// The architecture that was asked for.
        arch: &'static str,
    },
    /// The library has no `SONAME`.
    NoSoname,
    /// A name contains a zero byte, which a string table cannot hold.
    NameHasNul {
        /// The name, with the zero byte shown as `\0`.
        name: String,
    },
    /// A name is empty, which would collide with the string table's leading terminator.
    NameIsEmpty,
    /// Two symbols have the same name, so the description says two things about one name.
    Duplicate {
        /// The name that appears twice.
        name: String,
    },
    /// A function was given a size, or an object was given none.
    ///
    /// Neither is fatal to a linker and both mean the description was assembled wrongly, which is
    /// worth hearing about while there is still somebody to tell.
    SizeDisagrees {
        /// The name.
        name: String,
        /// What is wrong with it.
        because: &'static str,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotElf { target, format } => {
                write!(f, "{target} writes {format} rather than ELF, so it wants a different stub")
            }
            Error::NoMachineFlags { arch } => {
                write!(f, "what belongs in e_flags for {arch} is not decided yet")
            }
            Error::NoSoname => write!(f, "the library has no SONAME"),
            Error::NameHasNul { name } => write!(f, "`{name}` contains a zero byte"),
            Error::NameIsEmpty => write!(f, "a symbol has no name"),
            Error::Duplicate { name } => write!(f, "`{name}` is described twice"),
            Error::SizeDisagrees { name, because } => write!(f, "`{name}` {because}"),
        }
    }
}

impl std::error::Error for Error {}
