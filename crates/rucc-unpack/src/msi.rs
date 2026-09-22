//! The tables an MSI keeps in its streams, read.
//!
//! Design: `spec/cross-compile/13-distribution.md` section 13.4. An MSI is a relational database
//! laid out in the streams of a compound file, and [`crate::cfb`] is what gets the streams out. This
//! is what turns them into rows, and the rows are the only thing that can say which cabinet holds
//! which header and what that header is called inside the cabinet, because a cabinet calls
//! everything in it `fil` followed by a hash and says nothing else about it.
//!
//! # The names are encoded, and the alphabet is digits first
//!
//! A stream holding a table is not called `_Columns`. The format packs up to two characters of the
//! name into each UTF-16 code unit, six bits each, out of an alphabet of the digits, then the upper
//! case letters, then the lower case ones, then `.` and `_`. A code unit in `[0x3800, 0x4800)`
//! carries two characters, the low one first, one in `[0x4800, 0x4840)` carries a single character,
//! and `0x4840` is the marker that says the stream is a table, which is written `!` here. Anything
//! else stands for itself.
//!
//! Getting the alphabet the wrong way round is not an error, it is a name that decodes into
//! something else and a table that cannot be found, so it was taken off a real MSI rather than
//! guessed at: `_Columns` in a real one is `0x4840 0x3b3f 0x43f2 0x4438 0x45b1`, and that is only
//! `!_Columns` if `A` is at ten rather than at zero. The test pins those five numbers.
//!
//! # A table is column major and its schema is in another table
//!
//! The rows of a table are not laid out one after another. A table stream is every value of the
//! first column, then every value of the second, and nothing in the stream says how wide a row is
//! or how many rows there are: the widths come from `_Columns`, which is a table read the same way
//! with a schema this module has to know rather than read, and the row count is what is left over.
//!
//! No value in a table is text. A text column holds an index into the string pool, which is two
//! streams, one of lengths and one of the bytes end to end, and an integer column holds its value
//! with the high bit flipped so that a stored zero can mean null. Two bytes or four, and the low
//! byte of the column's type says which.
//!
//! # What is not here
//!
//! Writing, and the installer. An MSI carries the conditions, the sequences and the custom actions
//! that say what a Windows installation would do with these files, and none of that is a question a
//! cross compiler asks: it wants the files, the names they go under and the cabinets they are in.
//! The summary information stream is not a table and is not read.

use crate::cfb::{Cfb, CfbError};
use std::collections::BTreeMap;
use std::fmt;

/// The code unit that says a stream holds a table, and the character it is written as here.
const MARK: char = '!';

/// The type bit that says a column holds an index into the string pool rather than a number.
const TEXT: u16 = 0x0800;

/// The bit of the string pool's first entry that says a reference to it is three bytes rather than
/// two, which an MSI with more than 65,535 strings in it needs and no small one uses.
const LONG: u16 = 0x8000;

/// The codepage that means UTF-8.
const UTF8: u32 = 65001;

/// The codepage that means Windows 1252, which is Latin-1 except for the 32 characters below.
const WIN1252: u32 = 1252;

/// What Windows 1252 puts in `0x80` through `0x9f`, where Latin-1 has control characters. Written as
/// numbers rather than as the characters themselves because half of them are punctuation that looks
/// like other punctuation in a source file.
const HIGH: [u16; 32] = [
    0x20ac, 0, 0x201a, 0x0192, 0x201e, 0x2026, 0x2020, 0x2021, 0x02c6, 0x2030, 0x0160, 0x2039,
    0x0152, 0, 0x017d, 0, 0, 0x2018, 0x2019, 0x201c, 0x201d, 0x2022, 0x2013, 0x2014, 0x02dc,
    0x2122, 0x0161, 0x203a, 0x0153, 0, 0x017e, 0x0178,
];

/// Why an MSI could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MsiError {
    /// The compound file underneath could not be read.
    Cfb(CfbError),
    /// A stream, a table or a row this needs is not in the file.
    Missing {
        /// What was being looked for.
        what: String,
    },
    /// Something in the encoding is not what the format says it is.
    Malformed {
        /// Which part.
        what: &'static str,
    },
    /// A table has no such column, or has it holding the other kind of thing.
    Column {
        /// Which table.
        table: String,
        /// Which column.
        column: String,
    },
    /// The strings are in a codepage this does not decode.
    Codepage {
        /// Which one.
        which: u32,
    },
}

impl fmt::Display for MsiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MsiError::Cfb(why) => write!(f, "{why}"),
            MsiError::Missing { what } => write!(f, "this MSI has no {what} in it"),
            MsiError::Malformed { what } => write!(f, "{what}"),
            MsiError::Column { table, column } => {
                write!(f, "the {table} table has no {column} column of that kind")
            }
            MsiError::Codepage { which } => {
                write!(
                    f,
                    "the strings are in codepage {which}, and UTF-8 and 1252 are the ones read here"
                )
            }
        }
    }
}

impl std::error::Error for MsiError {}

impl From<CfbError> for MsiError {
    fn from(why: CfbError) -> MsiError {
        MsiError::Cfb(why)
    }
}

/// One column of a table, holding whichever of the two kinds of thing a column holds.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Held {
    Text(Vec<String>),
    Numbers(Vec<i64>),
}

/// One table of an MSI, read.
///
/// The columns are in the order the schema gives them, which is the order they sit in the stream,
/// and they are what a caller asks for: a table is stored a column at a time and every question
/// worth asking of one of these is a join, so there is nothing to be gained by handing back rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    name: String,
    rows: usize,
    columns: Vec<(String, Held)>,
}

impl Table {
    /// What the table is called.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// How many rows it has.
    #[must_use]
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// What its columns are called, in the order the schema puts them.
    #[must_use]
    pub fn columns(&self) -> Vec<&str> {
        self.columns.iter().map(|(name, _)| name.as_str()).collect()
    }

    /// One column of text, one entry per row.
    ///
    /// A null reads as the empty string, which is what the string pool's own entry zero is.
    ///
    /// # Errors
    ///
    /// [`MsiError::Column`] if there is no such column or it holds numbers.
    pub fn text(&self, column: &str) -> Result<&[String], MsiError> {
        match self.find(column) {
            Some(Held::Text(it)) => Ok(it),
            _ => Err(self.no(column)),
        }
    }

    /// One column of numbers, one entry per row.
    ///
    /// A null reads as zero. Every column this compiler reads for a number is one the schema says
    /// cannot be null, so the two cannot be confused here.
    ///
    /// # Errors
    ///
    /// [`MsiError::Column`] if there is no such column or it holds text.
    pub fn numbers(&self, column: &str) -> Result<&[i64], MsiError> {
        match self.find(column) {
            Some(Held::Numbers(it)) => Ok(it),
            _ => Err(self.no(column)),
        }
    }

    fn find(&self, column: &str) -> Option<&Held> {
        self.columns.iter().find(|(name, _)| name == column).map(|(_, held)| held)
    }

    fn no(&self, column: &str) -> MsiError {
        MsiError::Column { table: self.name.clone(), column: column.to_string() }
    }
}

/// One file an MSI says is in a cabinet, and where it goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Payload {
    /// The cabinet holding it, as the `Media` table spells it. Empty for a file that is loose on
    /// the media rather than in a cabinet, and a name starting with `#` is a stream inside the MSI
    /// itself rather than a file beside it.
    pub cabinet: String,
    /// What the cabinet calls it, which is a hash with `fil` or `cat` in front of it.
    pub key: String,
    /// What it is called where it is going.
    pub name: String,
    /// How many bytes it is.
    pub size: u64,
    /// The directory it goes in, with forward slashes and no leading one, relative to wherever the
    /// caller decides the install root is. Empty for a file that goes at the root itself.
    pub directory: String,
}

/// An MSI whose string pool and schema have been read.
#[derive(Debug)]
pub struct Msi {
    /// Entry zero is the empty string, which is what a null text value points at.
    strings: Vec<String>,
    /// How many bytes one reference into the pool takes, which is 2 or 3.
    refs: usize,
    /// The columns of each table, in the order the stream lays them out.
    schema: BTreeMap<String, Vec<(String, u16)>>,
    /// The table streams, by the decoded name with the marker taken off.
    streams: BTreeMap<String, Vec<u8>>,
}

impl Msi {
    /// Read the tables out of a compound file.
    ///
    /// # Errors
    ///
    /// [`MsiError`] for a file with no string pool or no schema in it, for a table stream that is
    /// not a whole number of rows wide, or for strings in a codepage this does not decode.
    pub fn read(file: &Cfb<'_>) -> Result<Msi, MsiError> {
        let mut streams = BTreeMap::new();
        for stream in file.streams() {
            // Only the tables. The summary information, the two signatures and the binary streams
            // that a custom action would run are not rows and are no business of this compiler's.
            let name = decode(&stream.name);
            if let Some(table) = name.strip_prefix(MARK) {
                streams.insert(table.to_string(), file.contents(stream)?);
            }
        }
        Msi::over(streams)
    }

    /// The same thing over the streams themselves, which is what [`Msi::read`] does once it has
    /// them and is how the tests get at this without laying out a compound file first.
    fn over(streams: BTreeMap<String, Vec<u8>>) -> Result<Msi, MsiError> {
        let pool = streams.get("_StringPool").ok_or_else(|| Msi::gone("_StringPool"))?;
        let data = streams.get("_StringData").ok_or_else(|| Msi::gone("_StringData"))?;
        let (strings, refs) = pool_of(pool, data)?;

        let mut msi = Msi { strings, refs, schema: BTreeMap::new(), streams };
        // The schema of the table that holds every other table's schema is the one thing here that
        // has to be known rather than read. These four are what every MSI's _Columns has.
        let columns = msi.plan(
            "_Columns",
            &[("Table", 0x0d48), ("Number", 0x0502), ("Name", 0x0d48), ("Type", 0x0502)],
        )?;
        let tables = columns.text("Table")?;
        let numbers = columns.numbers("Number")?;
        let names = columns.text("Name")?;
        let kinds = columns.numbers("Type")?;
        let mut schema: BTreeMap<String, Vec<(i64, String, u16)>> = BTreeMap::new();
        for n in 0..columns.rows() {
            let kind = u16::try_from(kinds[n] & 0xffff).expect("sixteen bits of it");
            schema.entry(tables[n].clone()).or_default().push((numbers[n], names[n].clone(), kind));
        }
        // The rows of _Columns are not in any order, and the number is what says where a column
        // sits in the stream, so a table read in the order the rows happened to arrive would read
        // every one of its columns out of the wrong place.
        msi.schema = schema
            .into_iter()
            .map(|(table, mut columns)| {
                columns.sort_by_key(|column| column.0);
                (table, columns.into_iter().map(|(_, name, kind)| (name, kind)).collect())
            })
            .collect();
        Ok(msi)
    }

    /// What tables this MSI has, which is what `_Columns` gave a schema for.
    #[must_use]
    pub fn tables(&self) -> Vec<&str> {
        self.schema.keys().map(String::as_str).collect()
    }

    /// One table, read.
    ///
    /// # Errors
    ///
    /// [`MsiError::Missing`] for a table this MSI has no schema for, and [`MsiError::Malformed`]
    /// for a stream that is not a whole number of rows wide.
    pub fn table(&self, name: &str) -> Result<Table, MsiError> {
        let schema = self.schema.get(name).ok_or_else(|| Msi::gone(name))?;
        let plan: Vec<(&str, u16)> =
            schema.iter().map(|(name, kind)| (name.as_str(), *kind)).collect();
        self.plan(name, &plan)
    }

    /// One table read against a schema given rather than looked up.
    fn plan(&self, name: &str, plan: &[(&str, u16)]) -> Result<Table, MsiError> {
        // A table with no rows in it has no stream at all rather than an empty one, which is not a
        // missing table: an MSI here lists Signature among its tables and ships no rows for it.
        let raw: &[u8] = self.streams.get(name).map_or(&[], Vec::as_slice);
        let mut widths = Vec::with_capacity(plan.len());
        for (_, kind) in plan {
            widths.push(self.width(*kind)?);
        }
        let row: usize = widths.iter().sum();
        if row == 0 {
            return Err(MsiError::Malformed { what: "a table with no columns in it" });
        }
        if raw.len() % row != 0 {
            return Err(MsiError::Malformed {
                what: "a table stream is not a whole number of rows",
            });
        }
        let rows = raw.len() / row;

        let mut columns = Vec::with_capacity(plan.len());
        let mut at = 0;
        for ((column, kind), width) in plan.iter().zip(widths) {
            let held = if kind & TEXT == 0 {
                let mut out = Vec::with_capacity(rows);
                for n in 0..rows {
                    let value = number(&raw[at + n * width..], width);
                    // A stored zero is a null and everything else carries the high bit flipped, so
                    // that null and the value zero are two different things on the wire.
                    let bias = 1i64 << (width * 8 - 1);
                    out.push(if value == 0 { 0 } else { value - bias });
                }
                Held::Numbers(out)
            } else {
                let mut out = Vec::with_capacity(rows);
                for n in 0..rows {
                    let which = usize::try_from(number(&raw[at + n * width..], width))
                        .expect("a width of two or three bytes is not negative");
                    out.push(self.strings.get(which).cloned().unwrap_or_default());
                }
                Held::Text(out)
            };
            at += rows * width;
            columns.push(((*column).to_string(), held));
        }
        Ok(Table { name: name.to_string(), rows, columns })
    }

    /// How many bytes one value of a column of this type takes.
    fn width(&self, kind: u16) -> Result<usize, MsiError> {
        if kind & TEXT != 0 {
            // The low byte of a text column is how long the longest string in it may be, which is
            // not what it takes on the wire: every one of them is an index into the pool.
            return Ok(self.refs);
        }
        match kind & 0xff {
            2 => Ok(2),
            4 => Ok(4),
            _ => {
                Err(MsiError::Malformed { what: "a column is neither text nor two or four bytes" })
            }
        }
    }

    /// Every file this MSI puts on disk, with the cabinet it comes out of and where it goes.
    ///
    /// This is the join the whole module exists for. `File` says what each file is called inside a
    /// cabinet and how far along the media it is, `Media` says which cabinet that reaches, and
    /// `Component` and `Directory` say where it lands, the second of them as a tree that has to be
    /// walked up to the root.
    ///
    /// # Errors
    ///
    /// [`MsiError`] for an MSI missing one of those four tables, for a row naming a component or a
    /// directory that is not there, and for a directory tree that points back at itself.
    pub fn payload(&self) -> Result<Vec<Payload>, MsiError> {
        let places = self.directories()?;

        let parts = self.table("Component")?;
        let of: BTreeMap<&str, &str> = parts
            .text("Component")?
            .iter()
            .map(String::as_str)
            .zip(parts.text("Directory_")?.iter().map(String::as_str))
            .collect();

        // The media rows are ranges of the sequence numbers the files are given, each named by
        // where it ends, so the cabinet holding a file is the first one that reaches it.
        let media = self.table("Media")?;
        let mut reach: Vec<(i64, &str)> = media
            .numbers("LastSequence")?
            .iter()
            .copied()
            .zip(media.text("Cabinet")?.iter().map(String::as_str))
            .collect();
        reach.sort_by_key(|one| one.0);

        let files = self.table("File")?;
        let keys = files.text("File")?;
        let components = files.text("Component_")?;
        let names = files.text("FileName")?;
        let sizes = files.numbers("FileSize")?;
        let order = files.numbers("Sequence")?;
        let mut out = Vec::with_capacity(files.rows());
        for n in 0..files.rows() {
            let component = components[n].as_str();
            let key = of.get(component).ok_or_else(|| Msi::gone(component))?;
            let directory = places.get(*key).ok_or_else(|| Msi::gone(key))?;
            let cabinet = reach.iter().find(|(last, _)| order[n] <= *last);
            out.push(Payload {
                cabinet: cabinet.map(|(_, name)| (*name).to_string()).unwrap_or_default(),
                key: keys[n].clone(),
                name: long(&names[n]).to_string(),
                size: u64::try_from(sizes[n]).unwrap_or(0),
                directory: directory.clone(),
            });
        }
        Ok(out)
    }

    /// Where each row of the `Directory` table puts things, as a path with forward slashes.
    fn directories(&self) -> Result<BTreeMap<String, String>, MsiError> {
        let table = self.table("Directory")?;
        let keys = table.text("Directory")?;
        let parents = table.text("Directory_Parent")?;
        let defaults = table.text("DefaultDir")?;
        let at: BTreeMap<&str, usize> =
            keys.iter().enumerate().map(|(n, key)| (key.as_str(), n)).collect();

        let mut out = BTreeMap::new();
        for (start, key) in keys.iter().enumerate() {
            let mut parts: Vec<&str> = Vec::new();
            let mut row = start;
            // Every row may be walked through once. A tree that points back at itself is refused
            // rather than walked forever, and the bound is what refuses it.
            let mut guard = table.rows() + 1;
            loop {
                guard = guard.checked_sub(1).ok_or(MsiError::Malformed {
                    what: "the directory tree points back at itself",
                })?;
                let parent = parents[row].as_str();
                // The row with no parent is the install root. Its own name is where the installer
                // would be told to put everything rather than a directory inside the tree, so it
                // is where the walk stops and it contributes nothing.
                if parent.is_empty() {
                    break;
                }
                let part = own(&defaults[row]);
                if !part.is_empty() && part != "." {
                    parts.push(part);
                }
                row = *at.get(parent).ok_or_else(|| Msi::gone(parent))?;
            }
            parts.reverse();
            out.insert(key.clone(), parts.join("/"));
        }
        Ok(out)
    }

    fn gone(what: &str) -> MsiError {
        MsiError::Missing { what: what.to_string() }
    }
}

/// The string pool, which is one stream of lengths and one of the bytes end to end.
///
/// The first entry is not a string. Its two halves are the codepage, and the bit that says how wide
/// a reference into the pool is, which an MSI with more strings than fit in two bytes needs.
fn pool_of(pool: &[u8], data: &[u8]) -> Result<(Vec<String>, usize), MsiError> {
    if pool.len() < 4 {
        return Err(MsiError::Malformed { what: "the string pool has no header" });
    }
    let head = |n: usize| u16::from_le_bytes([pool[n * 4], pool[n * 4 + 1]]);
    let refs = |n: usize| u16::from_le_bytes([pool[n * 4 + 2], pool[n * 4 + 3]]);
    let codepage = u32::from(head(0)) | (u32::from(refs(0) & !LONG) << 16);
    let wide = refs(0) & LONG != 0;

    let mut strings = vec![String::new()];
    let mut at = 0usize;
    let mut n = 1;
    while (n + 1) * 4 <= pool.len() {
        // A length of zero against a refcount that is not is not an empty string, it is a string
        // longer than 65,535 bytes whose length carries on into the next entry.
        let mut len = usize::from(head(n));
        if len == 0 && refs(n) != 0 {
            n += 1;
            if (n + 1) * 4 > pool.len() {
                return Err(MsiError::Malformed {
                    what: "a long string ran off the end of the pool",
                });
            }
            len = (usize::from(refs(n - 1)) << 16) | usize::from(head(n));
        }
        let bytes = data
            .get(at..at + len)
            .ok_or(MsiError::Malformed { what: "a string runs past the end of the string data" })?;
        strings.push(text(bytes, codepage)?);
        at += len;
        n += 1;
    }
    Ok((strings, if wide { 3 } else { 2 }))
}

/// Some bytes of the string data, in the codepage the pool named.
fn text(bytes: &[u8], codepage: u32) -> Result<String, MsiError> {
    match codepage {
        // Zero means the MSI never said, and every one that never says is ASCII, which both of
        // these decode the same way.
        UTF8 | 0 => String::from_utf8(bytes.to_vec())
            .map_err(|_| MsiError::Malformed { what: "a string is not the UTF-8 it says it is" }),
        WIN1252 => Ok(bytes
            .iter()
            .map(|&byte| match byte {
                0x80..=0x9f => char::from_u32(u32::from(HIGH[usize::from(byte) - 0x80]))
                    .unwrap_or(char::REPLACEMENT_CHARACTER),
                _ => char::from(byte),
            })
            .collect()),
        which => Err(MsiError::Codepage { which }),
    }
}

/// A little endian number of two, three or four bytes.
fn number(bytes: &[u8], width: usize) -> i64 {
    let mut out = 0i64;
    for n in 0..width {
        out |= i64::from(bytes.get(n).copied().unwrap_or(0)) << (8 * n);
    }
    out
}

/// The long half of a name the format allows to carry a short one in front of it.
///
/// `SHORT.EXT|The Long Name` is one name, not two, and the short one is there for a filesystem that
/// has not mattered since 1995.
fn long(name: &str) -> &str {
    name.rsplit('|').next().unwrap_or(name)
}

/// What one row of the directory table adds to the path under it.
///
/// A default directory is `target:source`, and each half may itself be `SHORT|Long`. The source
/// half says where the installer reads from, which is a fact about the installation and not about
/// where the file ends up, so only the target half is anything to do with this.
fn own(default: &str) -> &str {
    long(default.split(':').next().unwrap_or(default))
}

/// A stream name, decoded.
///
/// Anything that is not one of the three encoded forms stands for itself, which is how the streams
/// that are not tables keep the names a person would recognise.
fn decode(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for one in name.chars() {
        let value = one as u32;
        match value {
            0x3800..0x4800 => {
                let both = value - 0x3800;
                out.push(letter(both & 0x3f));
                out.push(letter((both >> 6) & 0x3f));
            }
            0x4800..0x4840 => out.push(letter(value - 0x4800)),
            0x4840 => out.push(MARK),
            _ => out.push(one),
        }
    }
    out
}

/// One of the sixty four characters a six bit piece of a name stands for.
fn letter(n: u32) -> char {
    match n {
        0..10 => char::from(b'0' + n as u8),
        10..36 => char::from(b'A' + (n - 10) as u8),
        36..62 => char::from(b'a' + (n - 36) as u8),
        62 => '.',
        _ => '_',
    }
}

#[cfg(test)]
mod tests {
    use super::{Msi, MsiError, decode};
    use std::collections::BTreeMap;

    /// A value to put in a test table, which is text, a number or a null.
    #[derive(Clone)]
    enum V {
        T(&'static str),
        N(i64),
        Null,
    }

    /// The pool a test MSI's strings go into as they are written.
    #[derive(Default)]
    struct Pool {
        strings: Vec<String>,
    }

    impl Pool {
        /// Where a string is, putting it in if it is not there yet. The empty string is entry zero
        /// and is never written down, the same as in a real one.
        fn of(&mut self, what: &str) -> i64 {
            if what.is_empty() {
                return 0;
            }
            if let Some(n) = self.strings.iter().position(|it| it == what) {
                return n as i64 + 1;
            }
            self.strings.push(what.to_string());
            self.strings.len() as i64
        }
    }

    /// One table of a test MSI: its name, its columns with their type words, and its rows.
    type Sketch = (&'static str, Vec<(&'static str, u16)>, Vec<Vec<V>>);

    /// Lay out the streams of an MSI by hand, with the string pool and the two metadata tables
    /// written from the tables given rather than alongside them.
    fn msi(tables: &[Sketch], codepage: u32) -> BTreeMap<String, Vec<u8>> {
        let mut pool = Pool::default();
        let mut out = BTreeMap::new();

        // _Tables is one column of names and _Columns is a row per column of every table, and both
        // of them are ordinary tables that go through the same writer as the rest.
        let listed: Vec<Vec<V>> = tables.iter().map(|(name, _, _)| vec![V::T(name)]).collect();
        let mut described: Vec<Vec<V>> = Vec::new();
        for (name, columns, _) in tables {
            for (n, (column, kind)) in columns.iter().enumerate() {
                described.push(vec![
                    V::T(name),
                    V::N(n as i64 + 1),
                    V::T(column),
                    V::N(i64::from(*kind)),
                ]);
            }
        }

        let write = |name: &str, columns: &[(&str, u16)], rows: &[Vec<V>], pool: &mut Pool| {
            let mut bytes = Vec::new();
            for (n, (_, kind)) in columns.iter().enumerate() {
                let width = if kind & super::TEXT != 0 { 2 } else { usize::from(*kind & 0xff) };
                for row in rows {
                    let value = match &row[n] {
                        V::T(what) => pool.of(what),
                        V::Null => 0,
                        V::N(it) => it + (1i64 << (width * 8 - 1)),
                    };
                    for byte in 0..width {
                        bytes.push(((value >> (8 * byte)) & 0xff) as u8);
                    }
                }
            }
            (name.to_string(), bytes)
        };

        let columns: Vec<(&str, u16)> =
            vec![("Table", 0x0d48), ("Number", 0x0502), ("Name", 0x0d48), ("Type", 0x0502)];
        let mut written = vec![
            write("_Tables", &[("Name", 0x0d48)], &listed, &mut pool),
            write("_Columns", &columns, &described, &mut pool),
        ];
        for (name, columns, rows) in tables {
            written.push(write(name, columns, rows, &mut pool));
        }

        // The pool itself is written last, because everything above is what put the strings in it.
        let mut lengths = Vec::new();
        let mut data = Vec::new();
        lengths.extend_from_slice(&(codepage as u16).to_le_bytes());
        lengths.extend_from_slice(&((codepage >> 16) as u16).to_le_bytes());
        for one in &pool.strings {
            let bytes = one.as_bytes();
            if bytes.len() > 0xffff {
                lengths.extend_from_slice(&0u16.to_le_bytes());
                lengths.extend_from_slice(&((bytes.len() >> 16) as u16).to_le_bytes());
                lengths.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
                lengths.extend_from_slice(&1u16.to_le_bytes());
            } else {
                lengths.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
                lengths.extend_from_slice(&1u16.to_le_bytes());
            }
            data.extend_from_slice(bytes);
        }
        out.insert("_StringPool".to_string(), lengths);
        out.insert("_StringData".to_string(), data);
        for (name, bytes) in written {
            // A table with no rows has no stream at all, which is what a real MSI does.
            if !bytes.is_empty() {
                out.insert(name, bytes);
            }
        }
        out
    }

    /// The four tables `payload` joins, with enough in them to join.
    fn whole() -> BTreeMap<String, Vec<u8>> {
        msi(
            &[
                (
                    "Media",
                    vec![("DiskId", 0x2502), ("LastSequence", 0x0104), ("Cabinet", 0x1dff)],
                    vec![
                        vec![V::N(1), V::N(2), V::T("first.cab")],
                        vec![V::N(2), V::N(4), V::T("second.cab")],
                    ],
                ),
                (
                    "File",
                    vec![
                        ("File", 0x2d48),
                        ("Component_", 0x0d48),
                        ("FileName", 0x0fff),
                        ("FileSize", 0x0104),
                        ("Sequence", 0x0104),
                    ],
                    vec![
                        vec![
                            V::T("fil1"),
                            V::T("cmp1"),
                            V::T("ASSERT~1.H|assert.h"),
                            V::N(1163),
                            V::N(1),
                        ],
                        vec![V::T("fil2"), V::T("cmp2"), V::T("ucrt.lib"), V::N(7), V::N(4)],
                    ],
                ),
                (
                    "Component",
                    vec![("Component", 0x2d48), ("Directory_", 0x0d48)],
                    vec![vec![V::T("cmp1"), V::T("ucrt")], vec![V::T("cmp2"), V::T("TARGETDIR")]],
                ),
                (
                    "Directory",
                    vec![
                        ("Directory", 0x2d48),
                        ("Directory_Parent", 0x1d48),
                        ("DefaultDir", 0x0fff),
                    ],
                    vec![
                        vec![V::T("TARGETDIR"), V::Null, V::T("SourceDir")],
                        vec![V::T("kits"), V::T("TARGETDIR"), V::T("WINDOW~1|Windows Kits")],
                        vec![V::T("here"), V::T("kits"), V::T(".")],
                        vec![V::T("ucrt"), V::T("here"), V::T("ucrt")],
                    ],
                ),
            ],
            65001,
        )
    }

    #[test]
    fn a_stream_name_is_two_characters_to_the_code_unit_and_the_alphabet_starts_at_the_digits() {
        // These five numbers are the name `_Columns` goes under in a real SDK MSI, and they decode
        // to that only if A is at ten. An alphabet with the letters first gives _Myv4wx2, which is
        // not an error anybody would see, it is a table that cannot be found.
        let real: String =
            ['\u{4840}', '\u{3b3f}', '\u{43f2}', '\u{4438}', '\u{45b1}'].into_iter().collect();
        assert_eq!(decode(&real), "!_Columns");
        // One character on its own, and something that is not encoded at all.
        assert_eq!(decode("\u{4800}\u{480a}\u{4824}"), "0Aa");
        assert_eq!(decode("Binary.WixCA"), "Binary.WixCA");
    }

    #[test]
    fn a_table_stream_is_column_major_and_its_schema_comes_from_another_table() {
        let msi = Msi::over(whole()).expect("an MSI this wrote itself");
        assert_eq!(msi.tables(), ["Component", "Directory", "File", "Media"]);
        let files = msi.table("File").expect("the table");
        assert_eq!(files.rows(), 2);
        assert_eq!(files.columns(), ["File", "Component_", "FileName", "FileSize", "Sequence"]);
        assert_eq!(files.text("File").expect("the keys"), ["fil1", "fil2"]);
        assert_eq!(files.numbers("FileSize").expect("the sizes"), [1163, 7]);
    }

    #[test]
    fn a_column_asked_for_as_the_wrong_kind_of_thing_is_refused() {
        let msi = Msi::over(whole()).expect("an MSI this wrote itself");
        let files = msi.table("File").expect("the table");
        assert!(matches!(files.numbers("File"), Err(MsiError::Column { .. })));
        assert!(matches!(files.text("FileSize"), Err(MsiError::Column { .. })));
        assert!(matches!(files.text("NoSuchThing"), Err(MsiError::Column { .. })));
        assert!(matches!(msi.table("NoSuchTable"), Err(MsiError::Missing { .. })));
    }

    #[test]
    fn a_number_carries_its_high_bit_flipped_so_a_stored_zero_can_be_a_null() {
        let one = msi(
            &[(
                "Thing",
                vec![("Small", 0x1502), ("Big", 0x1104)],
                vec![vec![V::N(0), V::N(-1)], vec![V::Null, V::Null]],
            )],
            65001,
        );
        // A stored zero is written for the null and the value zero is written as the bias itself,
        // so the two come back as what they are rather than as each other.
        let raw = one.get("Thing").expect("the stream");
        assert_eq!(raw[..2], 0x8000u16.to_le_bytes());
        assert_eq!(raw[2..4], [0, 0]);
        let msi = Msi::over(one).expect("an MSI this wrote itself");
        let thing = msi.table("Thing").expect("the table");
        assert_eq!(thing.numbers("Small").expect("the column"), [0, 0]);
        assert_eq!(thing.numbers("Big").expect("the column"), [-1, 0]);
    }

    #[test]
    fn a_table_with_no_rows_has_no_stream_at_all_and_is_not_a_missing_table() {
        // A real SDK MSI lists Signature among its tables and ships no stream for it, so a reader
        // that took a missing stream for a missing table would refuse to read that MSI.
        let mut one = whole();
        one.remove("Media");
        let msi = Msi::over(one).expect("an MSI this wrote itself");
        let media = msi.table("Media").expect("the table is still in the schema");
        assert_eq!(media.rows(), 0);
        assert!(media.text("Cabinet").expect("the column").is_empty());
    }

    #[test]
    fn a_stream_that_is_not_a_whole_number_of_rows_is_refused() {
        let mut one = whole();
        one.get_mut("File").expect("the stream").push(0);
        let msi = Msi::over(one).expect("the pool and the schema are still readable");
        assert!(matches!(msi.table("File"), Err(MsiError::Malformed { .. })));
    }

    #[test]
    fn a_string_longer_than_two_bytes_of_length_carries_on_into_the_next_entry() {
        let long = "x".repeat(70_000);
        let leaked: &'static str = Box::leak(long.clone().into_boxed_str());
        let one = msi(
            &[("Thing", vec![("Name", 0x0d48)], vec![vec![V::T(leaked)], vec![V::T("after")]])],
            65001,
        );
        let msi = Msi::over(one).expect("an MSI this wrote itself");
        let thing = msi.table("Thing").expect("the table");
        let names = thing.text("Name").expect("the column");
        assert_eq!(names[0].len(), 70_000);
        // And the entry after it is where it should be rather than one along, which is the half of
        // this that a reader gets wrong by not spending the second entry.
        assert_eq!(names[1], "after");
    }

    #[test]
    fn a_codepage_that_is_not_one_of_the_two_says_so_rather_than_guessing() {
        let one = msi(&[("Thing", vec![("Name", 0x0d48)], vec![vec![V::T("x")]])], 932);
        assert!(matches!(Msi::over(one), Err(MsiError::Codepage { which: 932 })));
        // And 1252 is read, where the byte that is a control character in Latin-1 is not one.
        let mut one = msi(&[("Thing", vec![("Name", 0x0d48)], vec![vec![V::T("x")]])], 1252);
        // The value is the last string to go into the pool, so its one byte is the last of the
        // data, and a byte that is a control character in Latin-1 goes in its place.
        let data = one.get_mut("_StringData").expect("the data");
        let last = data.len() - 1;
        data[last] = 0x93;
        let msi = Msi::over(one).expect("an MSI this wrote itself");
        assert_eq!(msi.table("Thing").expect("the table").text("Name").expect("it"), ["\u{201c}"]);
    }

    #[test]
    fn a_file_is_in_the_first_cabinet_that_reaches_its_sequence_number() {
        let msi = Msi::over(whole()).expect("an MSI this wrote itself");
        let payload = msi.payload().expect("the join");
        assert_eq!(payload.len(), 2);
        assert_eq!(payload[0].cabinet, "first.cab");
        assert_eq!(payload[1].cabinet, "second.cab");
        assert_eq!(payload[0].key, "fil1");
        assert_eq!(payload[0].size, 1163);
    }

    #[test]
    fn a_name_with_a_short_one_in_front_of_it_keeps_the_long_one() {
        let msi = Msi::over(whole()).expect("an MSI this wrote itself");
        let payload = msi.payload().expect("the join");
        assert_eq!(payload[0].name, "assert.h");
        assert_eq!(payload[1].name, "ucrt.lib");
    }

    #[test]
    fn a_directory_is_the_walk_up_to_the_row_with_no_parent() {
        let msi = Msi::over(whole()).expect("an MSI this wrote itself");
        let payload = msi.payload().expect("the join");
        // The root contributes nothing, a row whose default is a dot contributes nothing, and the
        // long half of a name with a short one in front of it is the half that is kept.
        assert_eq!(payload[0].directory, "Windows Kits/ucrt");
        assert_eq!(payload[1].directory, "");
    }

    #[test]
    fn a_directory_tree_that_points_back_at_itself_is_refused_rather_than_walked() {
        let one = msi(
            &[(
                "Directory",
                vec![("Directory", 0x2d48), ("Directory_Parent", 0x1d48), ("DefaultDir", 0x0fff)],
                vec![
                    vec![V::T("one"), V::T("two"), V::T("a")],
                    vec![V::T("two"), V::T("one"), V::T("b")],
                ],
            )],
            65001,
        );
        let msi = Msi::over(one).expect("an MSI this wrote itself");
        assert!(matches!(msi.directories(), Err(MsiError::Malformed { .. })));
    }

    #[test]
    fn a_row_naming_something_that_is_not_there_is_refused() {
        let one = msi(
            &[
                (
                    "Media",
                    vec![("LastSequence", 0x0104), ("Cabinet", 0x1dff)],
                    vec![vec![V::N(9), V::T("only.cab")]],
                ),
                (
                    "File",
                    vec![
                        ("File", 0x2d48),
                        ("Component_", 0x0d48),
                        ("FileName", 0x0fff),
                        ("FileSize", 0x0104),
                        ("Sequence", 0x0104),
                    ],
                    vec![vec![V::T("fil1"), V::T("gone"), V::T("a.h"), V::N(1), V::N(1)]],
                ),
                (
                    "Component",
                    vec![("Component", 0x2d48), ("Directory_", 0x0d48)],
                    vec![vec![V::T("cmp1"), V::T("TARGETDIR")]],
                ),
                (
                    "Directory",
                    vec![
                        ("Directory", 0x2d48),
                        ("Directory_Parent", 0x1d48),
                        ("DefaultDir", 0x0fff),
                    ],
                    vec![vec![V::T("TARGETDIR"), V::Null, V::T("SourceDir")]],
                ),
            ],
            65001,
        );
        let msi = Msi::over(one).expect("an MSI this wrote itself");
        assert!(matches!(msi.payload(), Err(MsiError::Missing { .. })));
    }

    #[test]
    fn an_msi_with_no_string_pool_in_it_says_so() {
        let mut one = whole();
        one.remove("_StringPool");
        assert!(matches!(Msi::over(one), Err(MsiError::Missing { .. })));
    }
}
