//! The compressed description, which is one file for every architecture and every glibc version.
//!
//! Design: `spec/cross-compile/09-libc-stubs.md` section 9.2, which says zig compresses the union of
//! glibc's `abilist` files across architectures and versions into a single binary blob of a few
//! hundred kilobytes, that we do the same, and that the format is ours. This is that format. Document
//! 13 section 13.1 budgets one megabyte for it and section 13.3 is the argument it exists to make:
//! stubs are generated rather than bundled because the cross product of eight architectures and a
//! dozen glibc versions is unbundlable, and what generates them has to be small enough to carry.
//!
//! # Why a version is a filter rather than a file
//!
//! An `abilist` line says which version node a name was added at, so glibc's current file already
//! contains every earlier release: what glibc 2.28 exported is what the file says with every node
//! above `GLIBC_2.28` dropped. [`Blob::exports_at`] is that, which is why the blob holds one section
//! per architecture and not one per architecture per version. It is also why document 13's row can
//! say "all architectures, all versions" and still be a megabyte.
//!
//! # What the shape of the real files is
//!
//! The eight `libc.abilist` files for the architectures rucc targets are 22712 lines and 589828
//! bytes of text between them, and three measurements off them decided the encoding:
//!
//! - There are 3000 distinct names in the union and 22712 lines, so a name is worth storing once and
//!   referring to by index. The names are 40 KB written out and 23 KB with each one storing only what
//!   it does not share with the name before it.
//! - No name is a function on one architecture and an object on another, in any of the eight files.
//!   So the kind lives once per name rather than once per line, and [`pack`] refuses a description
//!   that disagrees instead of keeping a copy per architecture for a case that does not arise.
//! - Only 92 of the 3000 names are data at all, so sizes are a rounding error and are stored per
//!   architecture, which they have to be: 65 of the 92 have a different size on some other
//!   architecture, because a great many of them are a structure holding a pointer.
//!
//! The version nodes are 55 distinct names and one of them dominates each architecture, since a port
//! lands in one release and nearly everything it exports is from that release: 2282 of aarch64's 2777
//! lines are at `GLIBC_2.17` and 2156 of loongarch64's 2298 are at `GLIBC_2.36`. A node is therefore
//! an index into a small table and nearly always the same index as the line before.
//!
//! # What compresses it
//!
//! Not a general purpose compressor. A name stores the length it shares with the name before it, a
//! name index stores its distance from the one before it, and a node is an index into a table of 55.
//! That is the whole of it, and the reason to stop there is that nothing has to be decompressed to
//! answer a question about one architecture: [`Blob::read`] decodes the tables, and an architecture's
//! symbols are decoded when they are asked for and skipped over otherwise.
//!
//! The eight real files come to 72316 bytes this way, which is 12 percent of the text they came from
//! and 7 percent of the megabyte document 13 budgets for them. A general purpose compressor over the
//! result would save more, and the room to do it is there if the other libcs ever need it.
//!
//! # What it is for
//!
//! Two stubs written from one blob, read back by `llvm-readelf`. At `GLIBC_2.39` the x86_64 section
//! gives 2818 symbols and 38 version definitions, with `memcpy@@GLIBC_2.14` and `memcpy@GLIBC_2.2.5`
//! behind it, and `pthread_create@@GLIBC_2.34` with `@GLIBC_2.2.5` behind it. At `GLIBC_2.12` the same
//! section gives 2310 symbols and 15 version definitions, and both of those names come out at
//! `GLIBC_2.2.5` as the definition an unversioned reference takes, because the newer ones had not
//! happened yet and `pthread_create` was still in `libpthread`. That difference is what cross
//! compiling is for, and it is one file.
//!
//! # Byte-identical output
//!
//! Claim 5 of `spec/cross-compile/02-the-goal.md` wants the same description to produce the same
//! bytes, so [`pack`] sorts everything it writes and the ordering does not depend on the order a
//! caller hands things over. The reader enforces the same rules the writer follows, which is less
//! about safety than about the claim: a table the reader accepted out of order, or a number spelled
//! with a longer encoding than it needs, would be a second spelling of one description, and then
//! "byte-identical" is a hope rather than a property.

use core::fmt;

use crate::abilist::Exports;
use crate::describe::{Binding, Clash, Kind, Symbol, Version, by_version, defaults, family};

/// What the first eight bytes of a blob are.
const MAGIC: &[u8; 8] = b"RUCCLIBC";

/// The format this module writes and the only one it reads.
///
/// A reader that met a number it did not know would have to guess at the layout, so it refuses. The
/// number is in the file rather than implied by the rucc version because document 13's cache keeps
/// artifacts across upgrades.
const FORMAT: u8 = 1;

/// The name is data rather than code.
const FLAG_OBJECT: u8 = 1 << 0;

/// A link can do without the name.
const FLAG_WEAK: u8 = 1 << 1;

/// Every flag bit this format defines, so that the rest can be refused rather than ignored.
const FLAGS: u8 = FLAG_OBJECT | FLAG_WEAK;

/// Writes the description of several architectures as one blob.
///
/// Takes an architecture name and what it exports, per architecture. The names are the caller's
/// vocabulary and nothing here reads them, so `x86_64` and `x86_64-linux-gnu` are both fine as long
/// as whoever reads the blob asks with the same spelling.
///
/// The input is what [`crate::abilist::read`] produced, which is the whole point: the description
/// comes from glibc, and this is the form it is carried in.
pub fn pack(descriptions: &[(&str, &Exports)]) -> Result<Vec<u8>, Error> {
    let architectures = sorted_architectures(descriptions)?;
    let names = name_table(&architectures)?;
    let nodes = node_table(&architectures);

    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.push(FORMAT);

    write_strings(&mut out, nodes.iter().map(String::as_str));

    varint(&mut out, names.len() as u64);
    let mut previous = "";
    for name in &names {
        string(&mut out, previous, &name.text);
        out.push(name.flags());
        previous = &name.text;
    }

    varint(&mut out, architectures.len() as u64);
    let mut previous = "";
    for (architecture, exports) in &architectures {
        string(&mut out, previous, architecture);
        previous = architecture;

        let entries = entry_table(architecture, exports, &names, &nodes)?;
        let mut body = Vec::new();
        let mut last = 0;
        for entry in &entries {
            varint(&mut body, (entry.name - last) as u64);
            last = entry.name;
            varint(&mut body, entry.node as u64);
            if names[entry.name].kind == Kind::Object {
                varint(&mut body, entry.size);
            }
        }
        varint(&mut out, entries.len() as u64);
        varint(&mut out, body.len() as u64);
        out.extend_from_slice(&body);
    }

    Ok(out)
}

/// The descriptions in the order they will be written, with a repeated architecture refused.
fn sorted_architectures<'a>(
    descriptions: &[(&'a str, &'a Exports)],
) -> Result<Vec<(&'a str, &'a Exports)>, Error> {
    let mut architectures = descriptions.to_vec();
    architectures.sort_by_key(|(architecture, _)| *architecture);
    for pair in architectures.windows(2) {
        let [(one, _), (two, _)] = *pair else { unreachable!("windows(2) gives pairs") };
        if one == two {
            return Err(Error::Repeated { architecture: one.to_owned() });
        }
    }
    Ok(architectures)
}

/// One name, stored once however many architectures export it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Name {
    /// The name itself.
    text: String,
    /// Code or storage, which no architecture disagrees about.
    kind: Kind,
    /// Whether a link can do without it.
    binding: Binding,
}

impl Name {
    /// The byte that carries everything about a name that is not its text.
    fn flags(&self) -> u8 {
        let mut flags = 0;
        if self.kind == Kind::Object {
            flags |= FLAG_OBJECT;
        }
        if self.binding == Binding::Weak {
            flags |= FLAG_WEAK;
        }
        flags
    }
}

/// Every name in the union, sorted, with the kind and binding each one agrees on.
///
/// Sorted with a list and a scan rather than a hash map, which this crate does not use: the names are
/// collected with the architecture each came from, sorted by name, and then each run of one name is
/// checked for agreement.
fn name_table(architectures: &[(&str, &Exports)]) -> Result<Vec<Name>, Error> {
    let mut seen: Vec<(&str, Kind, Binding, &str)> = Vec::new();
    for (architecture, exports) in architectures {
        for symbol in &exports.symbols {
            seen.push((&symbol.name, symbol.kind, symbol.binding, architecture));
        }
    }
    seen.sort_unstable_by(|one, two| one.0.cmp(two.0).then_with(|| one.3.cmp(two.3)));

    let mut names = Vec::new();
    let mut start = 0;
    while start < seen.len() {
        let mut end = start + 1;
        while end < seen.len() && seen[end].0 == seen[start].0 {
            end += 1;
        }
        let (name, kind, binding, architecture) = seen[start];
        for &(_, other_kind, other_binding, other) in &seen[start + 1..end] {
            if other_kind != kind {
                return Err(Error::Kinds {
                    name: name.to_owned(),
                    architectures: (architecture.to_owned(), other.to_owned()),
                });
            }
            if other_binding != binding {
                return Err(Error::Bindings {
                    name: name.to_owned(),
                    architectures: (architecture.to_owned(), other.to_owned()),
                });
            }
        }
        names.push(Name { text: name.to_owned(), kind, binding });
        start = end;
    }
    Ok(names)
}

/// Every version node in the union, in the order a person reads them.
///
/// Sorted by [`by_version`] rather than as text, so that a node's index and its version run the same
/// way and the table reads like `readelf -V` output when something has gone wrong and somebody is
/// looking at it.
fn node_table(architectures: &[(&str, &Exports)]) -> Vec<String> {
    let mut nodes: Vec<&str> = Vec::new();
    for (_, exports) in architectures {
        for symbol in &exports.symbols {
            if let Some(version) = &symbol.version {
                nodes.push(&version.node);
            }
        }
    }
    nodes.sort_unstable_by(|one, two| by_version(one, two));
    nodes.dedup();
    nodes.into_iter().map(str::to_owned).collect()
}

/// One line of one architecture's section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Entry {
    /// Which name, as an index into the name table.
    name: usize,
    /// Which node, as one more than an index into the node table, or zero for no node at all.
    node: usize,
    /// How many bytes of storage, which is only written for a name the table calls data.
    size: u64,
}

/// What one architecture exports, as indices, sorted and checked.
fn entry_table(
    architecture: &str,
    exports: &Exports,
    names: &[Name],
    nodes: &[String],
) -> Result<Vec<Entry>, Error> {
    let mut entries = Vec::with_capacity(exports.symbols.len());
    for symbol in &exports.symbols {
        let name = names
            .binary_search_by(|candidate| candidate.text.as_str().cmp(&symbol.name))
            .expect("the name table holds every name of every architecture");
        let node = match &symbol.version {
            None => 0,
            Some(version) => {
                if version.node == "GLIBC_PRIVATE" || version.node.starts_with("GLIBC_ABI_") {
                    // The blob carries what the ABI promises. `GLIBC_PRIVATE` is what glibc's own
                    // libraries call across the boundary and it moves between releases, and the
                    // `GLIBC_ABI_*` markers export nothing anybody calls. The abilist reader drops
                    // both, so one arriving here is a description built some other way and carrying
                    // it would put a symbol in a stub that no program may reference.
                    return Err(Error::Private {
                        architecture: architecture.to_owned(),
                        name: symbol.name.clone(),
                        node: version.node.clone(),
                    });
                }
                let index = nodes
                    .binary_search_by(|candidate| by_version(candidate, &version.node))
                    .expect("the node table holds every node of every architecture");
                index + 1
            }
        };
        if symbol.kind == Kind::Object && symbol.size == 0 {
            // A copy relocation against this would copy nothing into storage the program is about to
            // use. `crate::write` refuses it too, and refusing it here means a blob cannot be the
            // place a zero came from.
            return Err(Error::ZeroSize {
                architecture: architecture.to_owned(),
                name: symbol.name.clone(),
            });
        }
        entries.push(Entry { name, node, size: symbol.size });
    }

    entries.sort_unstable();
    for pair in entries.windows(2) {
        let [one, two] = *pair else { unreachable!("windows(2) gives pairs") };
        if one.name == two.name && one.node == two.node {
            return Err(Error::Twice {
                architecture: architecture.to_owned(),
                name: names[one.name].text.clone(),
                node: spell(nodes, one.node),
            });
        }
    }
    Ok(entries)
}

/// A node index as it should be read back, for a message.
fn spell(nodes: &[String], node: usize) -> String {
    match node {
        0 => "no version node".to_owned(),
        index => nodes[index - 1].clone(),
    }
}

/// A blob, with its tables decoded and its architecture sections left alone.
///
/// Borrows the bytes rather than taking them, because document 13's cache hands over a file and the
/// architecture somebody asked about is a small part of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blob<'a> {
    /// Every version node, in version order.
    nodes: Vec<String>,
    /// Every name, sorted, with its kind and binding.
    names: Vec<Name>,
    /// Where each architecture's symbols are, in the order they were written.
    architectures: Vec<Section<'a>>,
}

/// One architecture and the bytes of its symbols.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Section<'a> {
    /// The name the blob was written with.
    architecture: String,
    /// How many symbols the section holds.
    count: usize,
    /// The symbols, undecoded.
    body: &'a [u8],
}

impl<'a> Blob<'a> {
    /// Reads a blob's tables, leaving the architecture sections undecoded.
    pub fn read(bytes: &'a [u8]) -> Result<Self, Error> {
        let mut reader = Reader { bytes, at: 0 };
        if reader.take(MAGIC.len(), "the magic")? != MAGIC {
            return Err(Error::Magic);
        }
        let format = reader.byte("the format number")?;
        if format != FORMAT {
            return Err(Error::Format { found: format });
        }

        let nodes = read_strings(&mut reader, "a version node", by_version)?;
        let names = read_names(&mut reader)?;
        let architectures = read_sections(&mut reader)?;

        if reader.at != bytes.len() {
            return Err(Error::Trailing { found: bytes.len() - reader.at });
        }
        Ok(Blob { nodes, names, architectures })
    }

    /// Every architecture the blob describes, in the order it holds them.
    pub fn architectures(&self) -> impl ExactSizeIterator<Item = &str> {
        self.architectures.iter().map(|section| section.architecture.as_str())
    }

    /// Every version node the blob holds, lowest first.
    ///
    /// Across all architectures rather than one, since this is the table they share. An architecture's
    /// own nodes are the ones its symbols are at.
    pub fn nodes(&self) -> impl ExactSizeIterator<Item = &str> {
        self.nodes.iter().map(String::as_str)
    }

    /// What one architecture exports.
    ///
    /// Everything, which is what the glibc the blob was built from exports. For an older glibc, which
    /// is the ordinary case when cross compiling, [`Blob::exports_at`] is the one to call.
    ///
    /// The symbols come back sorted by name, and by node within a name, which is the order they are
    /// stored in and not the order the `abilist` file they came from listed them in. A blob has no
    /// lines, so there is no file order left to keep, and [`crate::write`] sorts what it is given
    /// anyway.
    pub fn exports(&self, architecture: &str) -> Result<Exports, Error> {
        self.decode(architecture, None)
    }

    /// What one architecture exported as of a version node.
    ///
    /// Keeps the nodes in `node`'s own family up to and including it, and every node in another
    /// family whatever it is: `GLIBC_2.28` says nothing about whether `GCC_3.0` had happened, and the
    /// four unwind symbols i386 and s390x export under that family are not glibc versioned. So
    /// `exports_at("x86_64", "GLIBC_2.28")` is what a program targeting CentOS 8 may refer to, and
    /// `memcpy` comes back at `GLIBC_2.2.5` as the default because the `GLIBC_2.14` definition is
    /// above the line and the rule that the highest node is the default is then reapplied to what is
    /// left.
    ///
    /// A node nothing is at is not an error. A target's glibc version is a number a person chose and
    /// there is no reason for the union of eight architectures to have a node at exactly it.
    pub fn exports_at(&self, architecture: &str, node: &str) -> Result<Exports, Error> {
        self.decode(architecture, Some(node))
    }

    /// Decodes one architecture's section, optionally dropping what a node is above.
    fn decode(&self, architecture: &str, ceiling: Option<&str>) -> Result<Exports, Error> {
        let section = self
            .architectures
            .iter()
            .find(|section| section.architecture == architecture)
            .ok_or_else(|| Error::Unknown { architecture: architecture.to_owned() })?;

        let mut reader = Reader { bytes: section.body, at: 0 };
        let mut symbols = Vec::with_capacity(section.count);
        let mut last = 0usize;
        for _ in 0..section.count {
            let step = reader.index("a name index")?;
            let name = last.checked_add(step).ok_or(Error::Overflow { what: "a name index" })?;
            last = name;
            let name = self.names.get(name).ok_or(Error::Name { index: name })?;

            let node = reader.index("a node index")?;
            let version = match node {
                0 => None,
                index => {
                    let node = self.nodes.get(index - 1).ok_or(Error::Node { index })?;
                    // Not the default until the whole section is in, for the same reason as in the
                    // abilist reader: the rule that decides it is about the other definitions of the
                    // same name, and which of those are here depends on the ceiling.
                    Some(Version { node: node.clone(), default: false })
                }
            };

            let size = match name.kind {
                Kind::Function => 0,
                Kind::Object => match reader.varint("a size")? {
                    0 => {
                        return Err(Error::ZeroSize {
                            architecture: architecture.to_owned(),
                            name: name.text.clone(),
                        });
                    }
                    size => size,
                },
            };

            if above(version.as_ref(), ceiling) {
                continue;
            }
            symbols.push(Symbol {
                name: name.text.clone(),
                kind: name.kind,
                binding: name.binding,
                version,
                size,
            });
        }
        if reader.at != section.body.len() {
            return Err(Error::Extra {
                architecture: architecture.to_owned(),
                found: section.body.len() - reader.at,
            });
        }

        defaults(&mut symbols).map_err(|clash| match clash {
            Clash::Duplicate { name, node, .. } => {
                Error::Twice { architecture: architecture.to_owned(), name, node }
            }
            Clash::Families { name, nodes } => Error::Families { name, nodes },
            Clash::Mixed { name } => Error::Mixed { architecture: architecture.to_owned(), name },
        })?;
        // Nothing was skipped, because a blob has no lines to skip and `pack` refuses the nodes the
        // abilist reader counts. The field is there for a file and this is not one.
        Ok(Exports { symbols, skipped: 0 })
    }
}

/// Whether a definition is above the node a caller asked about.
///
/// A definition with no node at all is never above one, since there is nothing to compare.
fn above(version: Option<&Version>, ceiling: Option<&str>) -> bool {
    let (Some(version), Some(ceiling)) = (version, ceiling) else {
        return false;
    };
    family(&version.node) == family(ceiling) && by_version(&version.node, ceiling).is_gt()
}

/// Writes a sorted table of strings, each storing only what it does not share with the one before it.
fn write_strings<'a>(out: &mut Vec<u8>, strings: impl ExactSizeIterator<Item = &'a str>) {
    varint(out, strings.len() as u64);
    let mut previous = "";
    for text in strings {
        string(out, previous, text);
        previous = text;
    }
}

/// Writes one string as the length it shares with the one before it and the rest of its bytes.
///
/// The shared length is in bytes and may land inside a character, which is fine because a reader puts
/// the bytes back together and checks the whole name rather than the part it just read.
fn string(out: &mut Vec<u8>, previous: &str, text: &str) {
    let shared =
        previous.as_bytes().iter().zip(text.as_bytes()).take_while(|(one, two)| one == two).count();
    varint(out, shared as u64);
    varint(out, (text.len() - shared) as u64);
    out.extend_from_slice(&text.as_bytes()[shared..]);
}

/// Writes a number in as few bytes as it takes, seven bits at a time.
fn varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

/// Reads a table of strings and checks it is in the order the writer would have put it in.
fn read_strings(
    reader: &mut Reader<'_>,
    what: &'static str,
    order: impl Fn(&str, &str) -> core::cmp::Ordering,
) -> Result<Vec<String>, Error> {
    let count = reader.index(what)?;
    let mut strings: Vec<String> = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        let text = reader.string(strings.last().map_or("", String::as_str), what)?;
        if let Some(previous) = strings.last() {
            if !order(previous, &text).is_lt() {
                return Err(Error::Order { what, one: previous.clone(), two: text });
            }
        }
        strings.push(text);
    }
    Ok(strings)
}

/// Reads the name table, which is a table of strings with a flag byte after each one.
fn read_names(reader: &mut Reader<'_>) -> Result<Vec<Name>, Error> {
    let what = "a name";
    let count = reader.index(what)?;
    let mut names: Vec<Name> = Vec::with_capacity(count.min(4096));
    for _ in 0..count {
        let text = reader.string(names.last().map_or("", |name| name.text.as_str()), what)?;
        if let Some(previous) = names.last() {
            if previous.text >= text {
                return Err(Error::Order { what, one: previous.text.clone(), two: text });
            }
        }
        let flags = reader.byte("a name's flags")?;
        if flags & !FLAGS != 0 {
            return Err(Error::Flags { name: text, flags });
        }
        let kind = if flags & FLAG_OBJECT == 0 { Kind::Function } else { Kind::Object };
        let binding = if flags & FLAG_WEAK == 0 { Binding::Global } else { Binding::Weak };
        names.push(Name { text, kind, binding });
    }
    Ok(names)
}

/// Reads the architecture directory, taking each section's bytes without looking at them.
fn read_sections<'a>(reader: &mut Reader<'a>) -> Result<Vec<Section<'a>>, Error> {
    let what = "an architecture";
    let count = reader.index(what)?;
    let mut sections: Vec<Section<'a>> = Vec::with_capacity(count.min(64));
    for _ in 0..count {
        let architecture =
            reader.string(sections.last().map_or("", |s| s.architecture.as_str()), what)?;
        if let Some(previous) = sections.last() {
            if previous.architecture >= architecture {
                return Err(Error::Order {
                    what,
                    one: previous.architecture.clone(),
                    two: architecture,
                });
            }
        }
        let count = reader.index("a symbol count")?;
        let length = reader.index("a section length")?;
        let body = reader.take(length, "an architecture's symbols")?;
        // One byte is the least a symbol can take, a name index of zero and a node index of zero, so
        // a count the bytes cannot hold is caught here rather than after a large allocation.
        if count > body.len() {
            return Err(Error::Count { architecture, count, length });
        }
        sections.push(Section { architecture, count, body });
    }
    Ok(sections)
}

/// A position in a blob.
struct Reader<'a> {
    /// The bytes being read.
    bytes: &'a [u8],
    /// How far in.
    at: usize,
}

impl<'a> Reader<'a> {
    /// Takes a run of bytes.
    fn take(&mut self, count: usize, what: &'static str) -> Result<&'a [u8], Error> {
        let end = self.at.checked_add(count).ok_or(Error::Overflow { what })?;
        let bytes = self.bytes.get(self.at..end).ok_or(Error::Truncated {
            what,
            wanted: count,
            left: self.bytes.len() - self.at,
        })?;
        self.at = end;
        Ok(bytes)
    }

    /// Takes one byte.
    fn byte(&mut self, what: &'static str) -> Result<u8, Error> {
        Ok(self.take(1, what)?[0])
    }

    /// Takes a number, refusing a spelling longer than the number needs.
    ///
    /// An overlong encoding is refused because the writer cannot produce one, so accepting it would
    /// be a second spelling of one description and claim 5 asks for there to be only one.
    fn varint(&mut self, what: &'static str) -> Result<u64, Error> {
        let mut value = 0u64;
        let mut shift = 0;
        loop {
            let byte = self.byte(what)?;
            let bits = u64::from(byte & 0x7f);
            if shift >= 64 || bits << shift >> shift != bits {
                return Err(Error::Overflow { what });
            }
            value |= bits << shift;
            if byte & 0x80 == 0 {
                if shift > 0 && byte == 0 {
                    return Err(Error::Overlong { what });
                }
                return Ok(value);
            }
            shift += 7;
        }
    }

    /// Takes a number that is a count or an index, which has to fit in one.
    fn index(&mut self, what: &'static str) -> Result<usize, Error> {
        usize::try_from(self.varint(what)?).map_err(|_| Error::Overflow { what })
    }

    /// Takes a string written against the one before it.
    fn string(&mut self, previous: &str, what: &'static str) -> Result<String, Error> {
        let shared = self.index(what)?;
        if shared > previous.len() {
            return Err(Error::Prefix { what, shared, available: previous.len() });
        }
        let length = self.index(what)?;
        let rest = self.take(length, what)?;
        let mut bytes = Vec::with_capacity(shared + length);
        bytes.extend_from_slice(&previous.as_bytes()[..shared]);
        bytes.extend_from_slice(rest);
        String::from_utf8(bytes).map_err(|_| Error::Utf8 { what })
    }
}

/// Why a description could not be packed, or a blob could not be read.
///
/// Every one of these is the input being wrong rather than the code failing, and the reading ones are
/// all about a blob that this module could not have written. A blob is a generated file and document
/// 13 has it content-addressed in a cache, so a surprising one is a sign something else is wrong and
/// the useful thing to do is say exactly what was surprising.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Two descriptions were handed over under one architecture name.
    Repeated {
        /// The name given twice.
        architecture: String,
    },
    /// One name is code on one architecture and storage on another.
    Kinds {
        /// The name.
        name: String,
        /// The two architectures that disagree.
        architectures: (String, String),
    },
    /// One name is global on one architecture and weak on another.
    Bindings {
        /// The name.
        name: String,
        /// The two architectures that disagree.
        architectures: (String, String),
    },
    /// A symbol is at a node the ABI does not promise.
    Private {
        /// Which architecture's description it was in.
        architecture: String,
        /// The name.
        name: String,
        /// The node.
        node: String,
    },
    /// An object symbol has a size of zero.
    ZeroSize {
        /// Which architecture.
        architecture: String,
        /// The name.
        name: String,
    },
    /// One name appears twice at one node on one architecture.
    Twice {
        /// Which architecture.
        architecture: String,
        /// The name.
        name: String,
        /// The node both definitions are at.
        node: String,
    },
    /// One name appears under two version families, so nothing orders its definitions.
    Families {
        /// The name.
        name: String,
        /// The two nodes.
        nodes: (String, String),
    },
    /// One name has a definition at a node and a definition with no node at all.
    Mixed {
        /// Which architecture.
        architecture: String,
        /// The name.
        name: String,
    },
    /// The first eight bytes are not this format's.
    Magic,
    /// The format number is not one this reads.
    Format {
        /// The number found.
        found: u8,
    },
    /// The blob ended in the middle of something.
    Truncated {
        /// What was being read.
        what: &'static str,
        /// How many bytes it wanted.
        wanted: usize,
        /// How many were left.
        left: usize,
    },
    /// A number does not fit, or a length would run past the end of the address space.
    Overflow {
        /// What was being read.
        what: &'static str,
    },
    /// A number is spelled with more bytes than it needs, which the writer never does.
    Overlong {
        /// What was being read.
        what: &'static str,
    },
    /// A string shares more with the one before it than that one has.
    Prefix {
        /// What was being read.
        what: &'static str,
        /// How much it claimed to share.
        shared: usize,
        /// How much there was.
        available: usize,
    },
    /// A string is not UTF-8 once put back together.
    Utf8 {
        /// What was being read.
        what: &'static str,
    },
    /// A table is not in the order the writer puts it in.
    Order {
        /// What the table holds.
        what: &'static str,
        /// The earlier entry.
        one: String,
        /// The entry after it.
        two: String,
    },
    /// A name's flag byte has a bit this format does not define.
    Flags {
        /// The name.
        name: String,
        /// The byte.
        flags: u8,
    },
    /// An architecture claims more symbols than its bytes could hold.
    Count {
        /// Which architecture.
        architecture: String,
        /// How many it claimed.
        count: usize,
        /// How many bytes it has.
        length: usize,
    },
    /// A symbol refers to a name the table does not have.
    Name {
        /// The index.
        index: usize,
    },
    /// A symbol refers to a node the table does not have.
    Node {
        /// The index, counting from one.
        index: usize,
    },
    /// An architecture's symbols did not use all of its bytes.
    Extra {
        /// Which architecture.
        architecture: String,
        /// How many bytes were left over.
        found: usize,
    },
    /// There are bytes after the last architecture.
    Trailing {
        /// How many.
        found: usize,
    },
    /// Nothing in the blob describes the architecture that was asked about.
    Unknown {
        /// The name asked about.
        architecture: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Repeated { architecture } => {
                write!(f, "two descriptions for `{architecture}`")
            }
            Error::Kinds { name, architectures: (one, two) } => write!(
                f,
                "`{name}` is a function on `{one}` and an object on `{two}`, or the other way \
                 round, and the blob keeps one kind per name"
            ),
            Error::Bindings { name, architectures: (one, two) } => write!(
                f,
                "`{name}` is global on `{one}` and weak on `{two}`, or the other way round, and \
                 the blob keeps one binding per name"
            ),
            Error::Private { architecture, name, node } => write!(
                f,
                "`{architecture}` has `{name}` at `{node}`, which is not part of the ABI and does \
                 not belong in a stub"
            ),
            Error::ZeroSize { architecture, name } => write!(
                f,
                "`{architecture}` gives the object `{name}` a size of zero, so a copy relocation \
                 against it would copy nothing"
            ),
            Error::Twice { architecture, name, node } => {
                write!(f, "`{architecture}` has `{name}` twice at `{node}`")
            }
            Error::Families { name, nodes: (one, two) } => write!(
                f,
                "`{name}` is at `{one}` and at `{two}`, which are different version families, so \
                 neither definition is the higher one"
            ),
            Error::Mixed { architecture, name } => write!(
                f,
                "`{architecture}` has `{name}` both at a version node and with none, so there is \
                 no saying which one an unversioned reference takes"
            ),
            Error::Magic => write!(f, "not a rucc libc description, by its first eight bytes"),
            Error::Format { found } => {
                write!(f, "description format {found}, and this reads format {FORMAT}")
            }
            Error::Truncated { what, wanted, left } => write!(
                f,
                "the description ends in the middle of {what}, which wanted {wanted} bytes with \
                 {left} left"
            ),
            Error::Overflow { what } => write!(f, "{what} is too large a number to be one"),
            Error::Overlong { what } => {
                write!(f, "{what} is spelled with more bytes than it needs")
            }
            Error::Prefix { what, shared, available } => write!(
                f,
                "{what} shares {shared} bytes with the entry before it, which has {available}"
            ),
            Error::Utf8 { what } => {
                write!(f, "{what} is not text once its shared prefix is put back on the front")
            }
            Error::Order { what, one, two } => {
                write!(f, "the table of {what} has `{one}` before `{two}`")
            }
            Error::Flags { name, flags } => {
                write!(
                    f,
                    "`{name}` has flags {flags:#04x}, which has a bit this format has no name for"
                )
            }
            Error::Count { architecture, count, length } => write!(
                f,
                "`{architecture}` claims {count} symbols in {length} bytes, and a symbol takes at \
                 least one"
            ),
            Error::Name { index } => {
                write!(f, "a symbol refers to name {index}, and there is no such name")
            }
            Error::Node { index } => {
                write!(f, "a symbol refers to node {index}, and there is no such node")
            }
            Error::Extra { architecture, found } => {
                write!(f, "`{architecture}` has {found} bytes after its last symbol")
            }
            Error::Trailing { found } => {
                write!(f, "{found} bytes after the last architecture")
            }
            Error::Unknown { architecture } => {
                write!(f, "nothing in the description is about `{architecture}`")
            }
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads an abilist, so that a test says what it means in the format the description comes in.
    fn exports(text: &str) -> Exports {
        crate::abilist::read(text).expect("the fixture is an abilist")
    }

    fn blob(descriptions: &[(&str, &Exports)]) -> Vec<u8> {
        pack(descriptions).expect("the fixtures are packable")
    }

    /// The same symbols in whatever order, since a blob keeps its own and not a file's.
    fn sorted(symbols: &[Symbol]) -> Vec<Symbol> {
        let mut symbols = symbols.to_vec();
        symbols.sort();
        symbols
    }

    #[test]
    fn a_description_comes_back_the_way_it_went_in() {
        let one = exports("GLIBC_2.2.5 printf F\nGLIBC_2.2.5 environ D 0x8\nGLIBC_2.14 memcpy F\n");
        let two = exports("GLIBC_2.17 printf F\nGLIBC_2.17 environ D 0x8\n");
        let bytes = blob(&[("x86_64", &one), ("aarch64", &two)]);

        let read = Blob::read(&bytes).expect("we wrote it");
        assert_eq!(read.architectures().collect::<Vec<_>>(), ["aarch64", "x86_64"]);
        let x86 = read.exports("x86_64").expect("it is in there");
        let arm = read.exports("aarch64").expect("it is in there");
        assert_eq!(sorted(&x86.symbols), sorted(&one.symbols));
        assert_eq!(sorted(&arm.symbols), sorted(&two.symbols));
    }

    #[test]
    fn the_order_descriptions_arrive_in_does_not_reach_the_bytes() {
        let one = exports("GLIBC_2.2.5 printf F\nGLIBC_2.14 memcpy F\n");
        let two = exports("GLIBC_2.17 printf F\n");
        assert_eq!(
            blob(&[("x86_64", &one), ("aarch64", &two)]),
            blob(&[("aarch64", &two), ("x86_64", &one)])
        );
    }

    #[test]
    fn a_second_architecture_costs_indices_rather_than_another_copy_of_the_names() {
        let names =
            ["pthread_mutexattr_setprotocol", "pthread_condattr_setclock", "__libc_start_main"];
        let text: String =
            names.iter().map(|name| format!("GLIBC_2.17 {name} F\n")).collect::<Vec<_>>().join("");
        let exports = exports(&text);

        let one = blob(&[("aarch64", &exports)]);
        let two = blob(&[("aarch64", &exports), ("riscv64", &exports)]);
        let spelled_out: usize = names.iter().map(|name| name.len()).sum();
        assert!(
            two.len() - one.len() < spelled_out,
            "a second architecture added {} bytes and its names are {spelled_out}",
            two.len() - one.len()
        );
    }

    #[test]
    fn the_highest_node_is_the_default() {
        let exports = exports("GLIBC_2.2.5 memcpy F\nGLIBC_2.14 memcpy F\n");
        let bytes = blob(&[("x86_64", &exports)]);
        let read = Blob::read(&bytes).expect("we wrote it");
        let symbols = read.exports("x86_64").expect("it is in there").symbols;

        let default: Vec<&str> = symbols
            .iter()
            .filter(|symbol| symbol.version.as_ref().is_some_and(|version| version.default))
            .map(|symbol| symbol.version.as_ref().expect("just checked").node.as_str())
            .collect();
        assert_eq!(default, ["GLIBC_2.14"]);
    }

    #[test]
    fn a_version_drops_the_nodes_above_it_and_the_default_moves_down() {
        let exports =
            exports("GLIBC_2.2.5 memcpy F\nGLIBC_2.14 memcpy F\nGLIBC_2.34 pthread_create F\n");
        let bytes = blob(&[("x86_64", &exports)]);
        let read = Blob::read(&bytes).expect("we wrote it");

        let old = read.exports_at("x86_64", "GLIBC_2.12").expect("it is in there");
        assert_eq!(old.symbols.len(), 1);
        assert_eq!(old.symbols[0].name, "memcpy");
        let version = old.symbols[0].version.as_ref().expect("it is versioned");
        assert_eq!(version.node, "GLIBC_2.2.5");
        assert!(
            version.default,
            "the only definition left has to be the one an unversioned reference takes"
        );
    }

    #[test]
    fn a_node_in_another_family_is_not_a_glibc_version_and_stays() {
        // i386 and s390x export four unwind symbols under `GCC_3.0`. A target glibc of 2.17 says
        // nothing about them, and dropping them would take four symbols out of every old target.
        let exports = exports("GCC_3.0 _Unwind_Find_FDE F\nGLIBC_2.34 pthread_create F\n");
        let bytes = blob(&[("i386", &exports)]);
        let read = Blob::read(&bytes).expect("we wrote it");

        let old = read.exports_at("i386", "GLIBC_2.17").expect("it is in there");
        assert_eq!(old.symbols.len(), 1);
        assert_eq!(old.symbols[0].name, "_Unwind_Find_FDE");
    }

    #[test]
    fn a_version_nothing_is_at_is_not_an_error() {
        let exports = exports("GLIBC_2.2.5 printf F\n");
        let bytes = blob(&[("x86_64", &exports)]);
        let read = Blob::read(&bytes).expect("we wrote it");
        assert_eq!(read.exports_at("x86_64", "GLIBC_2.31").expect("fine").symbols.len(), 1);
    }

    #[test]
    fn an_architecture_the_blob_has_nothing_about_is_refused() {
        let exports = exports("GLIBC_2.17 printf F\n");
        let bytes = blob(&[("aarch64", &exports)]);
        let read = Blob::read(&bytes).expect("we wrote it");
        assert_eq!(
            read.exports("sparc64"),
            Err(Error::Unknown { architecture: "sparc64".to_owned() })
        );
    }

    #[test]
    fn two_descriptions_for_one_architecture_are_refused() {
        let one = exports("GLIBC_2.17 printf F\n");
        assert_eq!(
            pack(&[("aarch64", &one), ("aarch64", &one)]),
            Err(Error::Repeated { architecture: "aarch64".to_owned() })
        );
    }

    #[test]
    fn a_name_that_is_a_function_on_one_architecture_and_an_object_on_another_is_refused() {
        let one = exports("GLIBC_2.2.5 environ D 0x8\n");
        let two = exports("GLIBC_2.17 environ F\n");
        assert_eq!(
            pack(&[("aarch64", &two), ("x86_64", &one)]),
            Err(Error::Kinds {
                name: "environ".to_owned(),
                architectures: ("aarch64".to_owned(), "x86_64".to_owned()),
            })
        );
    }

    #[test]
    fn a_name_that_is_weak_on_one_architecture_and_global_on_another_is_refused() {
        let mut one = exports("GLIBC_2.17 printf F\n");
        one.symbols[0].binding = Binding::Weak;
        let two = exports("GLIBC_2.2.5 printf F\n");
        assert_eq!(
            pack(&[("aarch64", &one), ("x86_64", &two)]),
            Err(Error::Bindings {
                name: "printf".to_owned(),
                architectures: ("aarch64".to_owned(), "x86_64".to_owned()),
            })
        );
    }

    #[test]
    fn a_private_node_is_refused() {
        let mut exports = exports("GLIBC_2.17 printf F\n");
        exports.symbols[0].version =
            Some(Version { node: "GLIBC_PRIVATE".to_owned(), default: true });
        assert_eq!(
            pack(&[("aarch64", &exports)]),
            Err(Error::Private {
                architecture: "aarch64".to_owned(),
                name: "printf".to_owned(),
                node: "GLIBC_PRIVATE".to_owned(),
            })
        );
    }

    #[test]
    fn an_object_of_no_size_is_refused() {
        let mut exports = exports("GLIBC_2.17 environ D 0x8\n");
        exports.symbols[0].size = 0;
        assert_eq!(
            pack(&[("aarch64", &exports)]),
            Err(Error::ZeroSize { architecture: "aarch64".to_owned(), name: "environ".to_owned() })
        );
    }

    #[test]
    fn one_name_twice_at_one_node_is_refused() {
        let mut exports = exports("GLIBC_2.17 printf F\n");
        exports.symbols.push(exports.symbols[0].clone());
        assert_eq!(
            pack(&[("aarch64", &exports)]),
            Err(Error::Twice {
                architecture: "aarch64".to_owned(),
                name: "printf".to_owned(),
                node: "GLIBC_2.17".to_owned(),
            })
        );
    }

    #[test]
    fn a_symbol_with_no_node_comes_back_with_none() {
        // musl and the BSDs have no version nodes at all, so this is their ordinary case.
        let exports = Exports {
            symbols: vec![Symbol::function("printf"), Symbol::object("environ", 8)],
            skipped: 0,
        };
        let bytes = blob(&[("x86_64", &exports)]);
        let read = Blob::read(&bytes).expect("we wrote it");
        let back = read.exports("x86_64").expect("it is in there");
        assert_eq!(sorted(&back.symbols), sorted(&exports.symbols));
    }

    #[test]
    fn something_that_is_not_a_description_is_refused() {
        assert_eq!(Blob::read(b"GLIBC_2.2.5 printf F\n"), Err(Error::Magic));
        assert_eq!(
            Blob::read(&[]),
            Err(Error::Truncated { what: "the magic", wanted: 8, left: 0 })
        );
    }

    #[test]
    fn a_format_number_this_does_not_read_is_refused() {
        let mut bytes = MAGIC.to_vec();
        bytes.push(FORMAT + 1);
        assert_eq!(Blob::read(&bytes), Err(Error::Format { found: FORMAT + 1 }));
    }

    #[test]
    fn a_blob_cut_short_anywhere_is_refused_rather_than_half_read() {
        let exports = exports("GLIBC_2.2.5 printf F\nGLIBC_2.2.5 environ D 0x8\n");
        let bytes = blob(&[("x86_64", &exports)]);
        for end in 0..bytes.len() {
            let read = Blob::read(&bytes[..end]);
            let short = match read {
                Err(_) => true,
                // A prefix that still parses has to have lost an architecture, because the tables
                // come first. Reading it is fine; quietly answering about x86_64 would not be.
                Ok(blob) => blob.exports("x86_64").is_err(),
            };
            assert!(short, "a blob cut at {end} of {} bytes read as whole", bytes.len());
        }
    }

    #[test]
    fn bytes_after_the_last_architecture_are_refused() {
        let exports = exports("GLIBC_2.2.5 printf F\n");
        let mut bytes = blob(&[("x86_64", &exports)]);
        bytes.push(0);
        assert_eq!(Blob::read(&bytes), Err(Error::Trailing { found: 1 }));
    }

    #[test]
    fn a_number_spelled_longer_than_it_needs_is_refused() {
        // The node table's count, 1, written as two bytes instead of one.
        let exports = exports("GLIBC_2.2.5 printf F\n");
        let bytes = blob(&[("x86_64", &exports)]);
        let mut longer = bytes[..9].to_vec();
        longer.extend_from_slice(&[0x81, 0x00]);
        longer.extend_from_slice(&bytes[10..]);
        assert_eq!(Blob::read(&longer), Err(Error::Overlong { what: "a version node" }));
    }

    #[test]
    fn a_table_out_of_order_is_refused() {
        // Two nodes written the wrong way round. Accepting it would mean the index of a node and its
        // version run opposite ways, and the rule that picks the default reads the table.
        let mut bytes = MAGIC.to_vec();
        bytes.push(FORMAT);
        write_strings(&mut bytes, ["GLIBC_2.14", "GLIBC_2.2.5"].into_iter());
        assert_eq!(
            Blob::read(&bytes),
            Err(Error::Order {
                what: "a version node",
                one: "GLIBC_2.14".to_owned(),
                two: "GLIBC_2.2.5".to_owned(),
            })
        );
    }

    #[test]
    fn a_string_sharing_more_than_the_one_before_it_has_is_refused() {
        let mut bytes = MAGIC.to_vec();
        bytes.push(FORMAT);
        varint(&mut bytes, 1);
        varint(&mut bytes, 3);
        varint(&mut bytes, 0);
        assert_eq!(
            Blob::read(&bytes),
            Err(Error::Prefix { what: "a version node", shared: 3, available: 0 })
        );
    }

    #[test]
    fn a_flag_bit_this_format_has_no_name_for_is_refused() {
        let mut bytes = MAGIC.to_vec();
        bytes.push(FORMAT);
        write_strings(&mut bytes, core::iter::empty());
        varint(&mut bytes, 1);
        string(&mut bytes, "", "printf");
        bytes.push(0x80);
        assert_eq!(
            Blob::read(&bytes),
            Err(Error::Flags { name: "printf".to_owned(), flags: 0x80 })
        );
    }

    #[test]
    fn an_architecture_claiming_more_symbols_than_it_has_bytes_is_refused() {
        let mut bytes = MAGIC.to_vec();
        bytes.push(FORMAT);
        write_strings(&mut bytes, core::iter::empty());
        varint(&mut bytes, 1);
        string(&mut bytes, "", "printf");
        bytes.push(0);
        varint(&mut bytes, 1);
        string(&mut bytes, "", "x86_64");
        varint(&mut bytes, 9);
        varint(&mut bytes, 2);
        bytes.extend_from_slice(&[0, 0]);
        assert_eq!(
            Blob::read(&bytes),
            Err(Error::Count { architecture: "x86_64".to_owned(), count: 9, length: 2 })
        );
    }

    #[test]
    fn a_symbol_pointing_at_a_name_that_is_not_there_is_refused() {
        let mut bytes = MAGIC.to_vec();
        bytes.push(FORMAT);
        write_strings(&mut bytes, core::iter::empty());
        varint(&mut bytes, 1);
        string(&mut bytes, "", "printf");
        bytes.push(0);
        varint(&mut bytes, 1);
        string(&mut bytes, "", "x86_64");
        varint(&mut bytes, 1);
        varint(&mut bytes, 2);
        bytes.extend_from_slice(&[7, 0]);
        let read = Blob::read(&bytes).expect("the tables are fine");
        assert_eq!(read.exports("x86_64"), Err(Error::Name { index: 7 }));
    }

    #[test]
    fn a_symbol_pointing_at_a_node_that_is_not_there_is_refused() {
        let mut bytes = MAGIC.to_vec();
        bytes.push(FORMAT);
        write_strings(&mut bytes, core::iter::empty());
        varint(&mut bytes, 1);
        string(&mut bytes, "", "printf");
        bytes.push(0);
        varint(&mut bytes, 1);
        string(&mut bytes, "", "x86_64");
        varint(&mut bytes, 1);
        varint(&mut bytes, 2);
        bytes.extend_from_slice(&[0, 3]);
        let read = Blob::read(&bytes).expect("the tables are fine");
        assert_eq!(read.exports("x86_64"), Err(Error::Node { index: 3 }));
    }

    #[test]
    fn every_error_says_something_a_person_can_act_on() {
        let errors = [
            Error::Repeated { architecture: "x86_64".to_owned() },
            Error::Kinds {
                name: "environ".to_owned(),
                architectures: ("a".to_owned(), "b".to_owned()),
            },
            Error::Bindings {
                name: "printf".to_owned(),
                architectures: ("a".to_owned(), "b".to_owned()),
            },
            Error::Private {
                architecture: "a".to_owned(),
                name: "n".to_owned(),
                node: "GLIBC_PRIVATE".to_owned(),
            },
            Error::ZeroSize { architecture: "a".to_owned(), name: "n".to_owned() },
            Error::Twice {
                architecture: "a".to_owned(),
                name: "n".to_owned(),
                node: "GLIBC_2.2.5".to_owned(),
            },
            Error::Families {
                name: "n".to_owned(),
                nodes: ("GCC_3.0".to_owned(), "GLIBC_2.0".to_owned()),
            },
            Error::Mixed { architecture: "a".to_owned(), name: "n".to_owned() },
            Error::Magic,
            Error::Format { found: 9 },
            Error::Truncated { what: "a name", wanted: 4, left: 1 },
            Error::Overflow { what: "a name" },
            Error::Overlong { what: "a name" },
            Error::Prefix { what: "a name", shared: 4, available: 1 },
            Error::Utf8 { what: "a name" },
            Error::Order { what: "a name", one: "b".to_owned(), two: "a".to_owned() },
            Error::Flags { name: "n".to_owned(), flags: 0x80 },
            Error::Count { architecture: "a".to_owned(), count: 9, length: 2 },
            Error::Name { index: 7 },
            Error::Node { index: 7 },
            Error::Extra { architecture: "a".to_owned(), found: 3 },
            Error::Trailing { found: 3 },
            Error::Unknown { architecture: "sparc64".to_owned() },
        ];
        for error in errors {
            let said = error.to_string();
            assert!(said.len() > 20, "{error:?} says only `{said}`");
            assert!(!said.ends_with('.'), "{error:?} ends in a full stop");
        }
    }
}
