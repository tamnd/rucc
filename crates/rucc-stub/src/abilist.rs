//! Reading glibc's `abilist` files, which is where a glibc description comes from.
//!
//! Design: `spec/cross-compile/09-libc-stubs.md` section 9.3.
//!
//! glibc checks one of these in per architecture and regenerates them from a built library with
//! `make update-abi`. A line is a version node, a name, a type letter, and a size for the types that
//! have one:
//!
//! ```text
//! GLIBC_2.2.5 printf F
//! GLIBC_2.2.5 environ D 0x8
//! GLIBC_2.14 memcpy F
//! ```
//!
//! That is the whole format. The reason to read it rather than a real `libc.so` is that this is the
//! file glibc itself treats as the ABI: a release that changes it changed the ABI on purpose, the
//! change is reviewed, and it is text, which matters both for section 9.6's size budget and for
//! anybody trying to see what a version bump actually did.
//!
//! # The two things the format does not say
//!
//! `scripts/abilist.awk` is the generator and it is short, which is worth knowing because both gaps
//! are visible in it rather than being a matter of opinion.
//!
//! It does not record the binding. The line it takes a symbol from is guarded by
//! `$2 == "g" || $2 == "w"`, global or weak, and it keeps neither letter, so every symbol read here
//! comes back [`Binding::Global`]. Describing a weak symbol as global is the last row of section
//! 9.1's table: it turns an optional symbol into a mandatory one, and the program that notices is the
//! one built against a libc that happens not to have it. Nothing in this module can answer it, and
//! section 9.8's comparison against a real `libc.so` is what would catch it.
//!
//! It does not record which definition of a name is the default. objdump prints a superseded
//! definition in parentheses, the generator's `gsub(/[()]/, "", version)` strips them, and what
//! survives is two lines for `memcpy` with nothing to tell them apart. [`read`] recovers it from
//! glibc's own construction: `versioned_symbol` puts a new implementation at a new node as the
//! default and `compat_symbol` leaves the old one behind, so the highest node a name appears at is
//! the default and the rest are superseded. That is what makes
//! [`crate::describe::by_version`] load bearing rather than cosmetic.
//!
//! # What it refuses
//!
//! The generator emits four type letters. `F` is a function and `D` is data with a size, and between
//! them they are every line of every `libc.abilist` for every architecture rucc targets: x86_64,
//! i386, aarch64, arm, riscv64, powerpc64le, s390 and loongarch64. `T` is data in `.tbss` or
//! `.tdata`, so thread local, and `O` is a powerpc64 function descriptor oddity. Neither appears in
//! any of those files and neither has an honest spelling in [`Symbol`] today, so both are refused by
//! name rather than quietly turned into ordinary data. Section 9.1's argument for refusing over
//! guessing is most of what makes the stub scheme trustworthy, and thread local storage described as
//! ordinary data is exactly the quiet wrongness it exists to prevent.

use core::fmt;

use crate::describe::{Binding, Kind, Symbol, Version, by_version};

/// What one `abilist` file says a library exports.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Exports {
    /// Every symbol, in the order the file listed them.
    ///
    /// File order rather than one of ours, because the file is `LC_ALL=C sort -u` output and keeping
    /// it means a person can read this beside the file line by line. [`crate::write`] sorts the
    /// symbols itself, so nothing about the bytes depends on the order here.
    pub symbols: Vec<Symbol>,
    /// How many lines were at a node the ABI does not promise.
    ///
    /// `GLIBC_PRIVATE`, which is where glibc puts what its own libraries call across the boundary and
    /// which moves between releases, and the `GLIBC_ABI_*` markers, which exist so a program can
    /// refuse to run against a dynamic linker too old for it and which export nothing anybody calls.
    /// The generator leaves both out unless asked, so a file straight from glibc has neither and this
    /// is zero. Counting them is how a caller can tell it was handed something generated with
    /// `include_private=1` instead.
    pub skipped: usize,
}

/// Reads an `abilist` file.
///
/// Takes the text rather than a path, because it may arrive from a sysroot on disk, from section
/// 9.6's compressed description, or from a string in a test, and none of that is this module's
/// business. Blank lines are allowed, since a real file has none and a fixture written by hand reads
/// better with them.
pub fn read(text: &str) -> Result<Exports, Error> {
    let mut symbols = Vec::new();
    // Which line each symbol came from, so that a problem only visible once every line is in can
    // still be reported at the line that caused it.
    let mut lines = Vec::new();
    let mut skipped = 0;

    for (index, line) in text.lines().enumerate() {
        // People and editors count lines from one.
        let line_number = index + 1;
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.is_empty() {
            continue;
        }
        let (node, name, letter, size) = match fields.as_slice() {
            [node, name, letter] => (*node, *name, *letter, None),
            [node, name, letter, size] => (*node, *name, *letter, Some(*size)),
            _ => return Err(Error::Fields { line: line_number, found: fields.len() }),
        };

        if node.contains('(') || node.contains(')') {
            // objdump's spelling for a superseded definition. The generator strips these, so a file
            // that still has them is objdump output rather than an abilist, and reading it as one
            // would turn `(GLIBC_2.2.5)` into a version node name of its own.
            return Err(Error::Parens { line: line_number, node: node.to_owned() });
        }
        if node == "GLIBC_PRIVATE" || node.starts_with("GLIBC_ABI_") {
            skipped += 1;
            continue;
        }

        let kind = match letter {
            "F" => Kind::Function,
            "D" => Kind::Object,
            "T" => {
                return Err(Error::Unwritable {
                    line: line_number,
                    letter: 'T',
                    what: "thread local, and nothing here writes a TLS segment yet",
                });
            }
            "O" => {
                return Err(Error::Unwritable {
                    line: line_number,
                    letter: 'O',
                    what: "a powerpc64 function descriptor, which wants handling section 9.1 has \
                           not settled",
                });
            }
            _ => return Err(Error::Letter { line: line_number, letter: letter.to_owned() }),
        };

        let size = match (kind, size) {
            (Kind::Function, None) => 0,
            (Kind::Function, Some(_)) => {
                return Err(Error::SizeOnFunction { line: line_number, name: name.to_owned() });
            }
            (Kind::Object, None) => {
                return Err(Error::NoSize { line: line_number, name: name.to_owned() });
            }
            (Kind::Object, Some(text)) => match hex(text) {
                // A copy relocation copies this many bytes, so a zero would mean the linker copies
                // nothing into storage a program is about to use. No libc abilist for any
                // architecture rucc targets has one, and `write` refuses it anyway, so saying so with
                // a line number is the more useful place to say it.
                Some(0) => {
                    return Err(Error::ZeroSize { line: line_number, name: name.to_owned() });
                }
                Some(size) => size,
                None => return Err(Error::Size { line: line_number, size: text.to_owned() }),
            },
        };

        symbols.push(Symbol {
            name: name.to_owned(),
            kind,
            // The format does not record it. See the module doc: this is section 9.1's last row and
            // it cannot be answered from here.
            binding: Binding::Global,
            // Not the default until the whole file has been read, because the rule that decides it
            // is about the other lines for the same name.
            version: Some(Version { node: node.to_owned(), default: false }),
            size,
        });
        lines.push(line_number);
    }

    defaults(&mut symbols, &lines)?;
    Ok(Exports { symbols, skipped })
}

/// A size as the generator writes it, which is `0x` and hex digits.
///
/// The digits are checked before parsing rather than left to [`u64::from_str_radix`], which accepts a
/// leading sign, so `0x+8` would otherwise be a size.
fn hex(size: &str) -> Option<u64> {
    let digits = size.strip_prefix("0x")?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(digits, 16).ok()
}

/// Marks the default definition of every name, which the file does not say.
///
/// The rule and the reason it is the rule are in the module doc. The work here is grouping the lines
/// for one name without a hash map, since this crate has none by policy, so a list of indices is
/// sorted and the symbols themselves stay in file order.
fn defaults(symbols: &mut [Symbol], lines: &[usize]) -> Result<(), Error> {
    let mut order: Vec<usize> = (0..symbols.len()).collect();
    order.sort_unstable_by(|&a, &b| {
        let (one, two) = (&symbols[a], &symbols[b]);
        one.name.cmp(&two.name).then_with(|| by_version(node(one), node(two)))
    });

    let mut start = 0;
    while start < order.len() {
        let mut end = start + 1;
        while end < order.len() && symbols[order[end]].name == symbols[order[start]].name {
            end += 1;
        }
        one_name(symbols, lines, &order[start..end])?;
        start = end;
    }
    Ok(())
}

/// Every definition of one name, lowest node first, with the highest made the default.
fn one_name(symbols: &mut [Symbol], lines: &[usize], group: &[usize]) -> Result<(), Error> {
    let name = symbols[group[0]].name.clone();
    for pair in group.windows(2) {
        let [before, after] = *pair else { unreachable!("windows(2) gives pairs") };
        let one = node(&symbols[before]);
        let two = node(&symbols[after]);
        if one == two {
            return Err(Error::Duplicate { line: lines[after], name, node: one.to_owned() });
        }
        if family(one) != family(two) {
            return Err(Error::Families { name, nodes: (one.to_owned(), two.to_owned()) });
        }
    }

    let highest = *group.last().expect("a group of one name has at least one line in it");
    let version = symbols[highest].version.as_mut().expect("read gives every symbol a node");
    version.default = true;
    Ok(())
}

/// The node a symbol read from an `abilist` is at.
///
/// Every symbol [`read`] produces has one, so this is an expectation rather than an option. A
/// function rather than a closure at each use, for the same reason as in [`crate::elf`]: a closure
/// taking a reference and returning one borrowed from it cannot name the lifetime that relates them.
fn node(symbol: &Symbol) -> &str {
    &symbol.version.as_ref().expect("read gives every symbol a node").node
}

/// The family a node name belongs to, which is everything before its last underscore.
///
/// `GLIBC_2.2.5` is in `GLIBC` and `GCC_3.0` is in `GCC`. The last underscore rather than the first,
/// because the version is the final component and a family name could contain one of its own.
fn family(node: &str) -> &str {
    match node.rsplit_once('_') {
        Some((family, _)) => family,
        None => node,
    }
}

/// Why an `abilist` file could not be read.
///
/// Every one of these is the file not being what this module reads, so none is recoverable by reading
/// more of it and all of them carry enough to open the file at the right place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A line is not a node, a name, a type letter and an optional size.
    Fields {
        /// The line, counting from one.
        line: usize,
        /// How many fields it had.
        found: usize,
    },
    /// A node name has parentheses, so this is objdump output rather than an `abilist`.
    Parens {
        /// The line, counting from one.
        line: usize,
        /// The node as written, parentheses included.
        node: String,
    },
    /// A type letter the generator does not produce.
    Letter {
        /// The line, counting from one.
        line: usize,
        /// The letter, as written, since it may not be one character.
        letter: String,
    },
    /// A type letter the generator produces and this crate has no honest spelling for.
    ///
    /// `T` and `O`. Both are refused rather than turned into data, and neither appears in the
    /// `libc.abilist` of any architecture rucc targets, so this is a gap that is real but not in the
    /// way of anything.
    Unwritable {
        /// The line, counting from one.
        line: usize,
        /// The letter.
        letter: char,
        /// What it means and why it is not written.
        what: &'static str,
    },
    /// A function was given a size, which the generator never does.
    SizeOnFunction {
        /// The line, counting from one.
        line: usize,
        /// The name.
        name: String,
    },
    /// A data symbol has no size, which the generator never does either.
    NoSize {
        /// The line, counting from one.
        line: usize,
        /// The name.
        name: String,
    },
    /// A size is not `0x` followed by hex digits.
    Size {
        /// The line, counting from one.
        line: usize,
        /// The size as written.
        size: String,
    },
    /// A data symbol has a size of zero, so a copy relocation against it would copy nothing.
    ZeroSize {
        /// The line, counting from one.
        line: usize,
        /// The name.
        name: String,
    },
    /// One name appears twice at one node, so the file says two things about one definition.
    ///
    /// The generator's output is `sort -u`, so two lines differing only in type or size are the only
    /// way to get here from a real file, and a file edited by hand is the likelier explanation.
    Duplicate {
        /// The line the second one is on, counting from one.
        line: usize,
        /// The name.
        name: String,
        /// The node both are at.
        node: String,
    },
    /// One name appears under two version families, so which definition is the default has no answer.
    ///
    /// The rule that recovers the default is that the highest node wins, and `GCC_3.0` is not higher
    /// or lower than `GLIBC_2.2.5` in any sense worth acting on. Real glibc does not do this: s390 is
    /// the only architecture whose `libc.abilist` has a `GCC_3.0` node at all, the four symbols at it
    /// appear nowhere else in the file, and no name in any architecture's file spans two families.
    Families {
        /// The name.
        name: String,
        /// The two nodes, in node order.
        nodes: (String, String),
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Fields { line, found } => write!(
                f,
                "line {line}: an abilist line is a node, a name, a type letter and a size for the \
                 types that have one, and this one has {found} fields"
            ),
            Error::Parens { line, node } => write!(
                f,
                "line {line}: the node `{node}` has parentheses, so this is objdump output rather \
                 than an abilist"
            ),
            Error::Letter { line, letter } => {
                write!(f, "line {line}: `{letter}` is not a type letter, which is F, D, T or O")
            }
            Error::Unwritable { line, letter, what } => {
                write!(f, "line {line}: `{letter}` is {what}")
            }
            Error::SizeOnFunction { line, name } => {
                write!(f, "line {line}: the function `{name}` was given a size")
            }
            Error::NoSize { line, name } => {
                write!(f, "line {line}: the data symbol `{name}` has no size")
            }
            Error::Size { line, size } => {
                write!(f, "line {line}: `{size}` is not a size, which is 0x followed by hex digits")
            }
            Error::ZeroSize { line, name } => write!(
                f,
                "line {line}: the data symbol `{name}` has a size of zero, so a copy relocation \
                 against it would copy nothing"
            ),
            Error::Duplicate { line, name, node } => {
                write!(f, "line {line}: `{name}` is already at {node} earlier in the file")
            }
            Error::Families { name, nodes } => write!(
                f,
                "`{name}` is at both {} and {}, which are different version families, so which \
                 definition an unversioned reference takes has no answer",
                nodes.0, nodes.1
            ),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Four lines in the shape glibc checks in, including one name at two nodes.
    const SOME: &str = "\
GLIBC_2.2.5 printf F
GLIBC_2.2.5 environ D 0x8
GLIBC_2.2.5 memcpy F
GLIBC_2.14 memcpy F
";

    fn node_of(exports: &Exports, at: usize) -> &str {
        exports.symbols[at].version.as_ref().unwrap().node.as_str()
    }

    #[test]
    fn a_function_has_no_size_and_a_data_symbol_has_one() {
        let exports = read(SOME).unwrap();
        assert_eq!(exports.symbols.len(), 4);
        assert_eq!(exports.skipped, 0);

        // File order, so this is line one.
        assert_eq!(exports.symbols[0].name, "printf");
        assert_eq!(exports.symbols[0].kind, Kind::Function);
        assert_eq!(exports.symbols[0].size, 0);

        assert_eq!(exports.symbols[1].name, "environ");
        assert_eq!(exports.symbols[1].kind, Kind::Object);
        assert_eq!(exports.symbols[1].size, 8);

        // The format records no binding at all, so this is what every symbol gets.
        assert!(exports.symbols.iter().all(|symbol| symbol.binding == Binding::Global));
    }

    #[test]
    fn the_highest_node_is_the_default() {
        let exports = read(SOME).unwrap();
        let default = |at: usize| exports.symbols[at].version.as_ref().unwrap().default;

        // The one name at one node is the default by being the only one.
        assert_eq!(node_of(&exports, 0), "GLIBC_2.2.5");
        assert!(default(0));

        // `memcpy` is at two, and the file says nothing about which an unversioned reference takes.
        assert_eq!(node_of(&exports, 2), "GLIBC_2.2.5");
        assert!(!default(2));
        assert_eq!(node_of(&exports, 3), "GLIBC_2.14");
        assert!(default(3));
    }

    #[test]
    fn highest_means_by_version_and_not_by_text() {
        // The case that makes the ordering load bearing. As text `GLIBC_2.9` is the greater of the
        // two, so a reader that sorted the strings would point every plain `f` at the older
        // implementation and nothing would say so.
        let exports = read("GLIBC_2.9 f F\nGLIBC_2.10 f F\n").unwrap();
        let default: Vec<&str> = exports
            .symbols
            .iter()
            .filter(|symbol| symbol.version.as_ref().unwrap().default)
            .map(|symbol| symbol.version.as_ref().unwrap().node.as_str())
            .collect();
        assert_eq!(default, ["GLIBC_2.10"]);
    }

    #[test]
    fn the_nodes_the_abi_does_not_promise_are_counted_and_left_out() {
        let text = "\
GLIBC_2.2.5 printf F
GLIBC_PRIVATE __libc_secret F
GLIBC_ABI_DT_RELR _marker F
";
        let exports = read(text).unwrap();
        assert_eq!(exports.symbols.len(), 1);
        assert_eq!(exports.symbols[0].name, "printf");
        assert_eq!(exports.skipped, 2);
    }

    #[test]
    fn blank_lines_are_not_a_problem_and_nothing_else_is_allowed() {
        assert_eq!(read("\nGLIBC_2.2.5 printf F\n\n").unwrap().symbols.len(), 1);
        assert_eq!(read("GLIBC_2.2.5 printf\n"), Err(Error::Fields { line: 1, found: 2 }));
        assert_eq!(
            read("GLIBC_2.2.5 printf F 0x8 extra\n"),
            Err(Error::Fields { line: 1, found: 5 })
        );
    }

    #[test]
    fn the_two_type_letters_with_no_home_here_are_refused_by_name() {
        // Refused rather than called data. Neither appears in the libc abilist of any architecture
        // rucc targets, so this is honest about a gap rather than in the way of one.
        let thread_local = read("GLIBC_2.2.5 f F\nGLIBC_2.18 tls T 0x8\n").unwrap_err();
        assert!(matches!(thread_local, Error::Unwritable { line: 2, letter: 'T', .. }));
        let descriptor = read("GLIBC_2.2.5 f O\n").unwrap_err();
        assert!(matches!(descriptor, Error::Unwritable { line: 1, letter: 'O', .. }));
        assert_eq!(
            read("GLIBC_2.2.5 f X\n"),
            Err(Error::Letter { line: 1, letter: "X".to_owned() })
        );
    }

    #[test]
    fn a_size_that_disagrees_with_the_type_is_refused() {
        assert_eq!(
            read("GLIBC_2.2.5 printf F 0x8\n"),
            Err(Error::SizeOnFunction { line: 1, name: "printf".to_owned() })
        );
        assert_eq!(
            read("GLIBC_2.2.5 environ D\n"),
            Err(Error::NoSize { line: 1, name: "environ".to_owned() })
        );
        assert_eq!(
            read("GLIBC_2.2.5 environ D 8\n"),
            Err(Error::Size { line: 1, size: "8".to_owned() })
        );
        // `from_str_radix` would take the sign, which is why the digits are checked first.
        assert_eq!(
            read("GLIBC_2.2.5 environ D 0x+8\n"),
            Err(Error::Size { line: 1, size: "0x+8".to_owned() })
        );
        assert_eq!(
            read("GLIBC_2.2.5 environ D 0x0\n"),
            Err(Error::ZeroSize { line: 1, name: "environ".to_owned() })
        );
    }

    #[test]
    fn objdump_output_is_not_an_abilist() {
        // Without this the parentheses become part of a node name and the stub defines a version
        // nobody will ever ask for.
        assert_eq!(
            read("(GLIBC_2.2.5) memcpy F\n"),
            Err(Error::Parens { line: 1, node: "(GLIBC_2.2.5)".to_owned() })
        );
    }

    #[test]
    fn one_name_at_one_node_twice_is_refused() {
        // The line reported is the second one, since that is the one a person would delete.
        assert_eq!(
            read("GLIBC_2.2.5 memcpy F\nGLIBC_2.14 memcpy F\nGLIBC_2.2.5 memcpy F\n"),
            Err(Error::Duplicate {
                line: 3,
                name: "memcpy".to_owned(),
                node: "GLIBC_2.2.5".to_owned(),
            })
        );
    }

    #[test]
    fn one_name_in_two_version_families_is_refused() {
        // s390's libc.abilist really does have a GCC_3.0 node beside its GLIBC ones, so the families
        // do coexist in one file. What does not happen is one name in both, and if it did there would
        // be no reading of which definition is newer.
        assert_eq!(
            read("GCC_3.0 _Unwind_Find_FDE F\nGLIBC_2.2.5 _Unwind_Find_FDE F\n"),
            Err(Error::Families {
                name: "_Unwind_Find_FDE".to_owned(),
                nodes: ("GCC_3.0".to_owned(), "GLIBC_2.2.5".to_owned()),
            })
        );
        // Two families in one file with no name in both is the real s390 shape and is fine.
        assert!(read("GCC_3.0 _Unwind_Find_FDE F\nGLIBC_2.2.5 printf F\n").is_ok());
    }

    #[test]
    fn the_order_lines_arrive_in_does_not_change_what_is_read() {
        // The file is sorted by node as text, which puts GLIBC_2.14 before GLIBC_2.2.5. Reading it
        // the other way round has to produce the same answer, since the ordering that decides the
        // default is this module's and not the file's.
        let forwards = read("GLIBC_2.14 memcpy F\nGLIBC_2.2.5 memcpy F\n").unwrap();
        let backwards = read("GLIBC_2.2.5 memcpy F\nGLIBC_2.14 memcpy F\n").unwrap();
        let defaults = |exports: &Exports| -> Vec<(String, bool)> {
            let mut out: Vec<(String, bool)> = exports
                .symbols
                .iter()
                .map(|symbol| {
                    let version = symbol.version.as_ref().unwrap();
                    (version.node.clone(), version.default)
                })
                .collect();
            out.sort();
            out
        };
        assert_eq!(defaults(&forwards), defaults(&backwards));
    }
}
