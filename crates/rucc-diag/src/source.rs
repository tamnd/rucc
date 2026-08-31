//! The source map: which file a [`BytePos`] lands in, and where in that file.
//!
//! Design: `spec/03-architecture.md`, and `spec/05-preprocessor.md` section 5.2 for the
//! coordinate space a token span lives in.
//!
//! Every file in a translation unit gets a range of one flat coordinate space, so a [`Span`]
//! is two integers and comparing two of them does not need to know which file each came from.
//! That is what keeps a token at sixteen bytes with a header nest twelve deep, and it is why
//! the expander can join the span of a macro argument with the span of the call site without
//! a special case.
//!
//! The price is that turning an offset back into a file, a line and a column is a search
//! rather than a field read. It is paid only when a diagnostic is rendered, which happens for
//! a handful of positions out of the millions the lexer produces, so the line table for a
//! file is built the first time somebody asks about that file and never for a file nobody
//! asks about.
//!
//! ```
//! use rucc_diag::SourceMap;
//!
//! let mut map = SourceMap::new();
//! let file = map.add("hello.c", b"int main(void)\n{\n    return 0;\n}\n".to_vec()).unwrap();
//! let brace = map.file(file).start + 15;
//! let loc = map.lookup(brace).unwrap();
//! assert_eq!(loc.line, 2);
//! assert_eq!(loc.column, 1);
//! assert_eq!(map.render_position(brace), "hello.c:2:1");
//! ```

use std::fmt;
use std::sync::{Arc, OnceLock};

use crate::{BytePos, Span};

/// The contents of a file, shared rather than copied.
///
/// A trait object rather than a `Vec`, so that the memory mapped input in
/// `spec/05-preprocessor.md` section 5.2 can be handed over as it is, and shared so that a
/// header included twice, or served twice out of the header cache, is held once.
///
/// It is a type of its own rather than a bare `Arc` so that it can have a `Debug` that says
/// how long a file is instead of printing it. A `{:#?}` of anything holding one of these
/// should not dump the whole of `stdio.h` into a test failure.
#[derive(Clone)]
pub struct SourceBytes(Arc<dyn AsRef<[u8]> + Send + Sync>);

impl SourceBytes {
    /// Takes ownership of anything that is a slice of bytes.
    pub fn new(bytes: impl AsRef<[u8]> + Send + Sync + 'static) -> SourceBytes {
        SourceBytes(Arc::new(bytes))
    }

    /// The bytes.
    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        (*self.0).as_ref()
    }
}

impl fmt::Debug for SourceBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SourceBytes({} bytes)", self.as_slice().len())
    }
}

impl AsRef<[u8]> for SourceBytes {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl std::ops::Deref for SourceBytes {
    type Target = [u8];

    #[inline]
    fn deref(&self) -> &[u8] {
        self.as_slice()
    }
}

/// A file in the source map.
///
/// Only meaningful against the map that issued it. A `FileId` is an index, so passing one to
/// a different map is a bug the type system does not catch, which is fine because there is
/// one map per compilation and it lives on the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FileId(u32);

impl FileId {
    /// The index of this file in the map, for a caller keeping a side table.
    #[inline]
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// A position resolved back to a human coordinate.
///
/// Lines and columns both count from one, because that is what every editor, every other
/// compiler and every user expects, and an off by one here is the kind of bug that survives
/// for years because nobody quite trusts their own arithmetic enough to file it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Loc {
    /// Which file.
    pub file: FileId,
    /// The line, counting from one.
    pub line: u32,
    /// The column in bytes, counting from one.
    ///
    /// Bytes rather than characters or display columns. A tab counts as one and a multibyte
    /// character counts as its encoded length, which is what a caret line drawn from the same
    /// bytes needs. `-ftabstop` and the width of a CJK character are the renderer's problem;
    /// what belongs here is the offset into the line.
    pub column: u32,
}

/// The bytes of one file, plus where they sit in the flat space.
///
/// Contents are held behind a trait object rather than as a `Vec`, so that the memory mapped
/// input in `spec/05-preprocessor.md` section 5.2 can be handed over as it is instead of
/// being copied into one. Reading the bytes goes through one virtual call, which is fine
/// because it happens once per file in the lexer and once per rendered diagnostic, never in
/// a loop.
pub struct SourceFile {
    /// This file's own id, so that anything holding a `&SourceFile` can name it.
    pub id: FileId,
    /// The name to print in a diagnostic, which is the path as the user wrote it rather than
    /// a canonical one. Somebody who typed `-I../include` wants to read `../include/foo.h`.
    pub name: String,
    /// First byte of this file in the flat space.
    pub start: BytePos,
    /// One past this file's last byte.
    pub end: BytePos,
    /// The `#include` that pulled this file in, or `None` for a file named on the command
    /// line. This is what "in file included from" is printed from.
    pub included_from: Option<Span>,
    bytes: SourceBytes,
    /// Absolute offset of the first byte of each line. Built on first use, because most files
    /// in a build are never the subject of a diagnostic.
    lines: OnceLock<Vec<BytePos>>,
}

impl fmt::Debug for SourceFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The bytes are deliberately not printed. A `{:#?}` of a session should not dump the
        // whole of `stdio.h` into a test failure.
        f.debug_struct("SourceFile")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("start", &self.start)
            .field("end", &self.end)
            .field("included_from", &self.included_from)
            .finish()
    }
}

impl SourceFile {
    /// The file's contents.
    #[inline]
    pub fn bytes(&self) -> &[u8] {
        self.bytes.as_slice()
    }

    /// The file's contents, shared.
    #[inline]
    pub fn shared_bytes(&self) -> SourceBytes {
        self.bytes.clone()
    }

    /// Length in bytes.
    #[inline]
    pub fn len(&self) -> u32 {
        self.end - self.start
    }

    /// Whether the file is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// Whether `pos` falls in this file.
    ///
    /// The end position is included, so a diagnostic about something missing at the end of a
    /// file still names the file rather than falling into the gap after it.
    #[inline]
    pub fn contains(&self, pos: BytePos) -> bool {
        self.start <= pos && pos <= self.end
    }

    /// How many lines the file has, counting a trailing newline as ending the last line
    /// rather than starting another. An empty file has one line, which is empty.
    pub fn line_count(&self) -> u32 {
        u32::try_from(self.lines().len()).unwrap_or(u32::MAX)
    }

    /// The bytes of line `line`, counting from one, without its line terminator.
    ///
    /// `None` if the file has no such line.
    pub fn line_bytes(&self, line: u32) -> Option<&[u8]> {
        let lines = self.lines();
        let index = usize::try_from(line.checked_sub(1)?).ok()?;
        let from = *lines.get(index)? - self.start;
        let to = lines.get(index + 1).map_or(self.len(), |next| *next - self.start);
        let text = self.bytes().get(from as usize..to as usize)?;
        // Strip the terminator rather than the last byte, so that a file with CRLF endings
        // does not put a carriage return in the middle of a rendered caret line.
        let text = text.strip_suffix(b"\n").unwrap_or(text);
        Some(text.strip_suffix(b"\r").unwrap_or(text))
    }

    /// The line and column of `pos`, or `None` if `pos` is not in this file.
    pub fn position(&self, pos: BytePos) -> Option<Loc> {
        let (line, begin) = self.line_of(pos)?;
        Some(Loc { file: self.id, line, column: pos - begin + 1 })
    }

    /// The span covering the line `pos` is on, including its terminator.
    pub fn line_span(&self, pos: BytePos) -> Option<Span> {
        let (line, begin) = self.line_of(pos)?;
        let end = self.lines().get(line as usize).copied().unwrap_or(self.end);
        Some(Span::new(begin, end))
    }

    /// The one-based line `pos` is on, and where that line starts.
    fn line_of(&self, pos: BytePos) -> Option<(u32, BytePos)> {
        if !self.contains(pos) {
            return None;
        }
        let lines = self.lines();
        // `partition_point` gives the number of line starts at or before `pos`, which is the
        // one-based line number, and is never zero because the first entry is the file start.
        let line = lines.partition_point(|&start| start <= pos);
        let begin = lines.get(line.saturating_sub(1)).copied().unwrap_or(self.start);
        Some((u32::try_from(line).unwrap_or(u32::MAX), begin))
    }

    /// The line start table, built on first use.
    fn lines(&self) -> &[BytePos] {
        self.lines.get_or_init(|| {
            let bytes = self.bytes();
            // Twenty four bytes a line is roughly what C source averages. Getting this wrong
            // costs a reallocation, not a correctness problem.
            let mut starts = Vec::with_capacity(bytes.len() / 24 + 1);
            starts.push(self.start);
            for (at, _) in bytes.iter().enumerate().filter(|&(_, &b)| b == b'\n') {
                let next = self.start + u32::try_from(at).unwrap_or(u32::MAX - 1) + 1;
                // A newline as the very last byte ends the last line, it does not open an
                // empty one. Every other newline opens a line, including one followed
                // immediately by another newline.
                if next < self.end {
                    starts.push(next);
                }
            }
            starts
        })
    }
}

/// The flat coordinate space is full.
///
/// Reaching this needs four gigabytes of source in one translation unit, counting every
/// header once per time it is included. It is reported rather than ignored because the
/// alternative is spans that silently point at the wrong file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceMapFull;

impl fmt::Display for SourceMapFull {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the translation unit does not fit in the four gigabyte source map")
    }
}

impl std::error::Error for SourceMapFull {}

/// Every file of one translation unit, laid end to end.
#[derive(Debug, Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
    next: BytePos,
}

impl SourceMap {
    /// An empty map.
    pub fn new() -> SourceMap {
        SourceMap::default()
    }

    /// Adds a file named on the command line.
    ///
    /// # Errors
    ///
    /// [`SourceMapFull`] if the file does not fit in what is left of the coordinate space.
    pub fn add(
        &mut self,
        name: impl Into<String>,
        bytes: impl AsRef<[u8]> + Send + Sync + 'static,
    ) -> Result<FileId, SourceMapFull> {
        self.push(name.into(), SourceBytes::new(bytes), None)
    }

    /// Adds a file whose contents are already shared.
    ///
    /// This is the entry point the file system abstraction uses, because it hands out bytes
    /// it may also be holding in a cache.
    ///
    /// # Errors
    ///
    /// [`SourceMapFull`] if the file does not fit in what is left of the coordinate space.
    pub fn add_shared(
        &mut self,
        name: impl Into<String>,
        bytes: SourceBytes,
        included_from: Option<Span>,
    ) -> Result<FileId, SourceMapFull> {
        self.push(name.into(), bytes, included_from)
    }

    /// Adds a file reached through the `#include` at `from`.
    ///
    /// # Errors
    ///
    /// [`SourceMapFull`] if the file does not fit in what is left of the coordinate space.
    pub fn add_included(
        &mut self,
        name: impl Into<String>,
        bytes: impl AsRef<[u8]> + Send + Sync + 'static,
        from: Span,
    ) -> Result<FileId, SourceMapFull> {
        self.push(name.into(), SourceBytes::new(bytes), Some(from))
    }

    fn push(
        &mut self,
        name: String,
        bytes: SourceBytes,
        included_from: Option<Span>,
    ) -> Result<FileId, SourceMapFull> {
        let len = u32::try_from(bytes.as_slice().len()).map_err(|_| SourceMapFull)?;
        let start = self.next;
        let end = start.checked_add(len).ok_or(SourceMapFull)?;
        // One byte of padding after every file, so that the position one past the end of a
        // file is still that file's and not the first byte of the next one. Without it a
        // diagnostic about a missing `}` at the end of a header names whatever came after it.
        // `BytePos::MAX` is `Span::DUMMY` and belongs to nobody, so the space stops one short.
        self.next = end.checked_add(1).filter(|&n| n < BytePos::MAX).ok_or(SourceMapFull)?;
        let id = FileId(u32::try_from(self.files.len()).map_err(|_| SourceMapFull)?);
        self.files.push(SourceFile {
            id,
            name,
            start,
            end,
            included_from,
            bytes,
            lines: OnceLock::new(),
        });
        Ok(id)
    }

    /// Every file, in the order they were added.
    pub fn files(&self) -> &[SourceFile] {
        &self.files
    }

    /// The file `id` names.
    ///
    /// # Panics
    ///
    /// Panics if `id` came from a different map. There is one map per compilation, on the
    /// session, so this is a programming error rather than something a caller handles.
    pub fn file(&self, id: FileId) -> &SourceFile {
        &self.files[id.index()]
    }

    /// Which file `pos` is in.
    pub fn lookup_file(&self, pos: BytePos) -> Option<FileId> {
        if pos == BytePos::MAX {
            return None;
        }
        // Files are laid out in increasing order and never overlap, so the candidate is the
        // last one starting at or before `pos`. It is a candidate rather than the answer
        // because `pos` may be in the padding byte after that file.
        let at = self.files.partition_point(|f| f.start <= pos);
        let file = self.files.get(at.checked_sub(1)?)?;
        file.contains(pos).then_some(file.id)
    }

    /// The file, line and column of `pos`.
    pub fn lookup(&self, pos: BytePos) -> Option<Loc> {
        self.file(self.lookup_file(pos)?).position(pos)
    }

    /// `name:line:column` for `pos`, or `<unknown>` for a position in no file.
    ///
    /// This is the prefix of a rendered diagnostic and the form every editor already knows
    /// how to jump to.
    pub fn render_position(&self, pos: BytePos) -> String {
        match self.lookup(pos) {
            Some(loc) => format!("{}:{}:{}", self.file(loc.file).name, loc.line, loc.column),
            None => "<unknown>".to_owned(),
        }
    }

    /// The chain of `#include` directives that led to `pos`, innermost first.
    ///
    /// Empty for a position in a file named on the command line. This is what the "in file
    /// included from" block of a diagnostic is printed from, and reading it out of the map
    /// rather than out of a stack the preprocessor keeps means it is still available long
    /// after preprocessing has finished.
    pub fn include_stack(&self, pos: BytePos) -> Vec<Span> {
        let mut stack = Vec::new();
        let mut at = self.lookup_file(pos);
        while let Some(file) = at {
            let Some(from) = self.file(file).included_from else { break };
            stack.push(from);
            at = self.lookup_file(from.lo);
            // A file is always added after the one that includes it, so the walk terminates.
            // A map built by hand in a test could say otherwise, and an infinite loop inside
            // the diagnostic renderer is a bad way to find that out.
            if stack.len() > self.files.len() {
                break;
            }
        }
        stack
    }

    /// How much of the coordinate space is used, which is where the next file will start.
    pub fn used(&self) -> BytePos {
        self.next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map_with(files: &[(&str, &str)]) -> (SourceMap, Vec<FileId>) {
        let mut map = SourceMap::new();
        let ids = files
            .iter()
            .map(|(name, text)| map.add(*name, text.as_bytes().to_vec()).unwrap())
            .collect();
        (map, ids)
    }

    #[test]
    fn the_first_file_starts_at_zero_and_the_next_one_after_a_gap() {
        let (map, ids) = map_with(&[("a.c", "ab"), ("b.c", "cd")]);
        assert_eq!(map.file(ids[0]).start, 0);
        assert_eq!(map.file(ids[0]).end, 2);
        assert_eq!(map.file(ids[1]).start, 3);
        assert_eq!(map.used(), 6);
    }

    #[test]
    fn the_position_after_a_file_belongs_to_that_file_and_not_the_next() {
        let (map, ids) = map_with(&[("a.c", "ab"), ("b.c", "cd")]);
        assert_eq!(map.lookup_file(2), Some(ids[0]));
        assert_eq!(map.lookup_file(3), Some(ids[1]));
    }

    #[test]
    fn a_position_in_the_gap_is_in_no_file() {
        let mut map = SourceMap::new();
        map.add("a.c", b"ab".to_vec()).unwrap();
        // Offset 2 is the end of `a.c`, and offset 3 would be the next file, which does not
        // exist, so nothing is there.
        assert_eq!(map.lookup_file(3), None);
        assert_eq!(map.render_position(3), "<unknown>");
    }

    #[test]
    fn a_dummy_span_resolves_to_nothing() {
        let (map, _) = map_with(&[("a.c", "ab")]);
        assert_eq!(map.lookup(Span::DUMMY.lo), None);
        assert_eq!(map.lookup_file(BytePos::MAX), None);
    }

    #[test]
    fn lines_and_columns_count_from_one() {
        let (map, ids) = map_with(&[("a.c", "one\ntwo\nthree\n")]);
        let start = map.file(ids[0]).start;
        assert_eq!(map.lookup(start).unwrap(), Loc { file: ids[0], line: 1, column: 1 });
        assert_eq!(map.lookup(start + 4).unwrap(), Loc { file: ids[0], line: 2, column: 1 });
        assert_eq!(map.lookup(start + 6).unwrap(), Loc { file: ids[0], line: 2, column: 3 });
        assert_eq!(map.render_position(start + 8), "a.c:3:1");
    }

    #[test]
    fn a_trailing_newline_does_not_open_a_line() {
        let (map, ids) = map_with(&[("a.c", "one\ntwo\n"), ("b.c", "one\ntwo")]);
        assert_eq!(map.file(ids[0]).line_count(), 2);
        assert_eq!(map.file(ids[1]).line_count(), 2);
    }

    #[test]
    fn a_blank_line_is_a_line() {
        let (map, ids) = map_with(&[("a.c", "one\n\nthree\n")]);
        let file = map.file(ids[0]);
        assert_eq!(file.line_count(), 3);
        assert_eq!(file.line_bytes(2), Some(&b""[..]));
        assert_eq!(file.line_bytes(3), Some(&b"three"[..]));
        assert_eq!(file.line_bytes(4), None);
        assert_eq!(file.line_bytes(0), None);
    }

    #[test]
    fn a_carriage_return_is_not_part_of_the_line() {
        let (map, ids) = map_with(&[("a.c", "one\r\ntwo\r\n")]);
        let file = map.file(ids[0]);
        assert_eq!(file.line_bytes(1), Some(&b"one"[..]));
        assert_eq!(file.line_bytes(2), Some(&b"two"[..]));
    }

    #[test]
    fn an_empty_file_has_one_position_and_no_lines_to_read() {
        let (map, ids) = map_with(&[("a.c", "")]);
        let file = map.file(ids[0]);
        assert!(file.is_empty());
        assert_eq!(map.lookup(file.start).unwrap().line, 1);
        assert_eq!(file.line_bytes(1), Some(&b""[..]));
        assert_eq!(file.line_bytes(2), None);
    }

    #[test]
    fn a_line_span_covers_the_terminator() {
        let (map, ids) = map_with(&[("a.c", "one\ntwo\n")]);
        let file = map.file(ids[0]);
        assert_eq!(file.line_span(file.start + 1), Some(Span::new(0, 4)));
        assert_eq!(file.line_span(file.start + 5), Some(Span::new(4, 8)));
    }

    #[test]
    fn the_include_stack_runs_from_the_innermost_out() {
        let mut map = SourceMap::new();
        let main = map.add("main.c", b"#include <a.h>\n".to_vec()).unwrap();
        let outer = Span::new(map.file(main).start, map.file(main).start + 14);
        let a = map.add_included("a.h", b"#include <b.h>\n".to_vec(), outer).unwrap();
        let inner = Span::new(map.file(a).start, map.file(a).start + 14);
        let b = map.add_included("b.h", b"int x;\n".to_vec(), inner).unwrap();
        let stack = map.include_stack(map.file(b).start);
        assert_eq!(stack, vec![inner, outer]);
        assert_eq!(map.lookup(stack[0].lo).unwrap().file, a);
        assert_eq!(map.lookup(stack[1].lo).unwrap().file, main);
        assert!(map.include_stack(outer.lo).is_empty());
    }

    #[test]
    fn a_file_that_does_not_fit_is_refused_rather_than_wrapped() {
        let mut map = SourceMap::new();
        map.add("a.c", b"x".to_vec()).unwrap();
        // The map is then asked for everything that is left plus the padding it always adds,
        // which is one byte more than the space holds.
        map.next = BytePos::MAX - 2;
        assert_eq!(map.add("b.c", b"xx".to_vec()), Err(SourceMapFull));
        assert_eq!(map.files().len(), 1);
    }

    #[test]
    fn contents_can_be_anything_that_is_a_slice_of_bytes() {
        // What a memory mapped file will look like when it arrives: not a `Vec`, not a
        // `String`, just something that hands out a slice.
        struct Mapped(&'static [u8]);
        impl AsRef<[u8]> for Mapped {
            fn as_ref(&self) -> &[u8] {
                self.0
            }
        }
        let mut map = SourceMap::new();
        let id = map.add("a.c", Mapped(b"int x;\n")).unwrap();
        assert_eq!(map.file(id).bytes(), b"int x;\n");
        assert_eq!(map.file(id).line_count(), 1);
    }
}
