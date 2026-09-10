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

/// The version node one definition of a name belongs to.
///
/// This is the third row of section 9.1's table and the one with no shortcut, per section 9.2. glibc
/// exports several implementations of the same name under different nodes, `memcpy@GLIBC_2.2.5`
/// beside `memcpy@GLIBC_2.14`, and a link binds to the highest node not exceeding the target's glibc
/// version. That is what makes building on a new machine and running on an old one work, and it is
/// why `x86_64-linux-gnu.2.28` and `x86_64-linux-gnu.2.39` are different targets rather than
/// spellings of one.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    /// The node's name, as `GLIBC_2.17`.
    pub node: String,
    /// Whether an unversioned reference to the name binds here.
    ///
    /// Exactly one definition of a name is the default and the rest are superseded. A reference
    /// written `memcpy` takes the default, and one written `memcpy@GLIBC_2.2.5` takes the node it
    /// names, which is how a program linked years ago keeps the implementation it was built against.
    pub default: bool,
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
    /// The version node this definition belongs to, or [`None`] for a library that has none.
    ///
    /// musl and the BSDs have no version nodes at all, so [`None`] is the ordinary case for them
    /// rather than a gap. On glibc a symbol without one is a real thing too, since not everything
    /// glibc exports is versioned.
    pub version: Option<Version>,
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
        Symbol {
            name: name.into(),
            kind: Kind::Function,
            binding: Binding::Global,
            size: 0,
            version: None,
        }
    }

    /// A global object of a given size.
    pub fn object(name: impl Into<String>, size: u64) -> Self {
        Symbol {
            name: name.into(),
            kind: Kind::Object,
            binding: Binding::Global,
            size,
            version: None,
        }
    }

    /// The same symbol, weak.
    #[must_use]
    pub fn weak(mut self) -> Self {
        self.binding = Binding::Weak;
        self
    }

    /// The same symbol, at a version node, as the definition an unversioned reference binds to.
    #[must_use]
    pub fn at(mut self, node: impl Into<String>) -> Self {
        self.version = Some(Version { node: node.into(), default: true });
        self
    }

    /// The same symbol, at a version node something newer has taken the default away from.
    ///
    /// This is the older implementation that stays exported so that a program built against it keeps
    /// resolving, and only a reference naming the node explicitly reaches it. Spelled as its own
    /// constructor rather than as a flag applied after [`Symbol::at`], because a flag that quietly
    /// does nothing when the version has not been set yet is a description bug with no symptom.
    #[must_use]
    pub fn behind(mut self, node: impl Into<String>) -> Self {
        self.version = Some(Version { node: node.into(), default: false });
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

/// Orders two version node names the way a person reading `readelf -V` would expect.
///
/// Digit runs compare as numbers and everything else compares as bytes, which is enough for
/// `GLIBC_x.y`, `GCC_x.y` and `FBSD_1.x` without knowing anything about any of them. The reason not
/// to just sort the strings is that `GLIBC_2.10` sorts before `GLIBC_2.2.5` as text and after it as a
/// version.
///
/// [`crate::write`] uses this to order the `.gnu.version_d` chain, where nothing in the format
/// depends on it: each record carries its own index and a reader follows `vd_next`, so any order
/// produces a correct file, and the only loss from a strange one is that section 9.8's highest value
/// test is a person diffing our table against a real `libc.so`.
///
/// [`crate::abilist`] depends on it for a fact though, because an `abilist` file does not record
/// which definition of a name is the default and the rule that recovers it is that the highest node
/// is. So this is load bearing rather than cosmetic, and an ordering that got `GLIBC_2.10` and
/// `GLIBC_2.9` the wrong way round would silently point unversioned references at the older
/// implementation.
///
/// Names that come out equal run by run fall back to comparing their bytes, so this is a total order
/// and not just nearly one. Without that, `A1` and `A01` are equal here and unequal to [`str`], which
/// lets an unstable sort put a third name between two copies of one name and a [`Vec::dedup`] after
/// it keep both. Two records for one node is not a reading anybody would enjoy tracking down.
pub fn by_version(a: &str, b: &str) -> std::cmp::Ordering {
    let mut left = runs(a);
    let mut right = runs(b);
    loop {
        match (left.next(), right.next()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(one), Some(two)) => {
                let order = match (one.parse::<u64>(), two.parse::<u64>()) {
                    (Ok(one), Ok(two)) => one.cmp(&two),
                    _ => one.cmp(two),
                };
                if order != std::cmp::Ordering::Equal {
                    return order;
                }
            }
        }
    }
}

/// Splits a name into runs of digits and runs of everything else.
fn runs(name: &str) -> impl Iterator<Item = &str> {
    let mut rest = name;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let digits = rest.starts_with(|c: char| c.is_ascii_digit());
        let end = rest.find(|c: char| c.is_ascii_digit() != digits).unwrap_or(rest.len());
        let (run, tail) = rest.split_at(end);
        rest = tail;
        Some(run)
    })
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
    ///
    /// A name may appear more than once only when every appearance carries a version node, since
    /// that is the one case where the appearances are telling a linker apart rather than
    /// contradicting each other.
    Duplicate {
        /// The name that appears twice.
        name: String,
    },
    /// A name has more than one default version, so an unversioned reference has no single answer.
    TwoDefaults {
        /// The name.
        name: String,
        /// The two nodes that both claim it, in the order they are written out.
        nodes: (String, String),
    },
    /// A version node has no name, which a string table cannot tell from the absence of one.
    EmptyNode {
        /// The symbol whose node is nameless.
        name: String,
    },
    /// A name appears both with a version node and without one.
    ///
    /// The unversioned definition would answer every reference the versioned one exists to answer,
    /// so the description is saying two incompatible things about how the name resolves.
    MixedVersioning {
        /// The name.
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
            Error::TwoDefaults { name, nodes } => write!(
                f,
                "`{name}` is the default at both {} and {}, so an unversioned reference to it has \
                 two answers",
                nodes.0, nodes.1
            ),
            Error::EmptyNode { name } => {
                write!(f, "the version node `{name}` belongs to has no name")
            }
            Error::MixedVersioning { name } => {
                write!(f, "`{name}` is described both with a version node and without one")
            }
            Error::SizeDisagrees { name, because } => write!(f, "`{name}` {because}"),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use super::by_version;

    #[test]
    fn version_nodes_order_the_way_a_person_reads_them() {
        // The case that makes this worth having at all. As text `GLIBC_2.10` is less than
        // `GLIBC_2.2.5`, because `1` is less than `2`, and as a version it is greater.
        assert_eq!(by_version("GLIBC_2.10", "GLIBC_2.2.5"), Ordering::Greater);
        assert_eq!(by_version("GLIBC_2.2.5", "GLIBC_2.14"), Ordering::Less);
        assert_eq!(by_version("GLIBC_2.34", "GLIBC_2.4"), Ordering::Greater);
        // A prefix sorts before what extends it, so a node and a point release of it stay in order.
        assert_eq!(by_version("GLIBC_2.2", "GLIBC_2.2.5"), Ordering::Less);
        // Different families sort by their names, which is all anybody needs of them.
        assert_eq!(by_version("GCC_3.0", "GLIBC_2.2.5"), Ordering::Less);
        assert_eq!(by_version("GLIBC_2.17", "GLIBC_2.17"), Ordering::Equal);
    }

    #[test]
    fn two_spellings_of_one_number_are_not_equal() {
        // Equal here and unequal to `str` is the combination that breaks the dedup in the writer's
        // node list, since an unstable sort may then put a third name between two copies of one name.
        assert_ne!(by_version("A01", "A1"), Ordering::Equal);
        assert_ne!(by_version("GLIBC_2.02", "GLIBC_2.2"), Ordering::Equal);
    }
}
