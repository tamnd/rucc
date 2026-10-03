//! The line table, which is what `.file` and `.loc` are for.
//!
//! gcc under `-g` writes `.debug_info` itself and leaves `.debug_line` empty, with a `.loc` in front
//! of the instructions each line of the source became. The assembler is the one that knows where
//! those instructions ended up, so it keeps a row for every `.loc` and writes the table out at the
//! end. This is that, done the way gas 2.44 does it in `dwarf2dbg.c`, because the point is an
//! object that comes out byte for byte the same as the one gas would have made.
//!
//! A row is made when the next instruction is laid down, or at once when the `.loc` names a view.
//! A view counts rows at one address: it goes back to zero when the address moves on and up by one
//! when it does not, and the name a `.loc` gives it is set to that number, which is how gcc's
//! location lists say which of several rows at one address a variable's location starts at.
//! `view -0` asks for zero whatever the address did, and the table makes that true by setting the
//! address again, which is the one place the table depends on how gas cut the section into pieces.
//! See [`Lines::variable`].

use rucc_base::hash::Map;

use crate::unwind::{sleb, uleb};

/// What the line number opcodes are, by the numbers DWARF gives them.
const COPY: u8 = 1;
const ADVANCE_PC: u8 = 2;
const ADVANCE_LINE: u8 = 3;
const SET_FILE: u8 = 4;
const SET_COLUMN: u8 = 5;
const NEGATE_STMT: u8 = 6;
const SET_BASIC_BLOCK: u8 = 7;
const CONST_ADD_PC: u8 = 8;
const SET_PROLOGUE_END: u8 = 10;
const SET_EPILOGUE_BEGIN: u8 = 11;
const SET_ISA: u8 = 12;
const END_SEQUENCE: u8 = 1;
const SET_ADDRESS: u8 = 2;
const SET_DISCRIMINATOR: u8 = 4;

/// The shape gas gives every special opcode: the line moves by `LINE_BASE` up to `LINE_BASE +
/// LINE_RANGE - 1`, and the opcodes start after the thirteen standard ones.
const LINE_BASE: i64 = -5;
const LINE_RANGE: u64 = 14;
const OPCODE_BASE: u64 = 13;
/// The furthest one special opcode moves the address, and what `DW_LNS_const_add_pc` adds.
const MOST_SPECIAL: u64 = (255 - OPCODE_BASE) / LINE_RANGE;

/// `DW_FORM_line_strp`, `DW_FORM_udata` and `DW_FORM_data16`, and the three columns of the
/// version five file table gas writes.
const LINE_STRP: u64 = 0x1f;
const UDATA: u64 = 0x0f;
const DATA16: u64 = 0x1e;
const PATH: u64 = 1;
const DIRECTORY: u64 = 2;
const MD5: u64 = 5;

/// What the last `.loc` said, which the next row is made from.
#[derive(Debug, Clone)]
pub(crate) struct Loc {
    pub(crate) file: u64,
    pub(crate) line: u64,
    pub(crate) column: u64,
    pub(crate) stmt: bool,
    pub(crate) isa: u64,
    pub(crate) discriminator: u64,
    pub(crate) basic_block: bool,
    pub(crate) prologue_end: bool,
    pub(crate) epilogue_begin: bool,
    /// The name `view` gave, for the row to set to its view number.
    pub(crate) view: Option<String>,
    /// Whether `view` was `-0`, which starts the count again however the address moved.
    pub(crate) reset: bool,
}

impl Default for Loc {
    fn default() -> Loc {
        Loc {
            file: 1,
            line: 1,
            column: 0,
            stmt: true,
            isa: 0,
            discriminator: 0,
            basic_block: false,
            prologue_end: false,
            epilogue_begin: false,
            view: None,
            reset: false,
        }
    }
}

/// One row of the table.
#[derive(Debug, Clone)]
pub(crate) struct Row {
    /// The section the row is in and how far into it, which moves when a subsection is joined on to
    /// the section it belongs to.
    pub(crate) part: usize,
    pub(crate) at: u64,
    /// Where the row was when it was made: the section, which piece of it, and how far in.
    home: (usize, usize, u64),
    loc: Loc,
}

/// Where gas cut one section into pieces, which it does at every alignment, every jump that may
/// grow and every `.org`.
#[derive(Debug, Default)]
struct Pieces {
    /// Where each piece starts.
    starts: Vec<u64>,
    /// Where the fixed part of each piece that has been cut off ends, which is where the thing of
    /// variable size that cut it off starts.
    ends: Vec<u64>,
}

/// One entry of the file table.
#[derive(Debug, Clone)]
struct File {
    name: Vec<u8>,
    dir: usize,
    md5: [u8; 16],
}

/// A place in the table that the linker fills in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Hole {
    /// How far into the table.
    pub(crate) at: usize,
    pub(crate) width: u8,
    pub(crate) to: To,
}

/// What a [`Hole`] holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum To {
    /// The address of a place in a section.
    Code { part: usize, at: u64 },
    /// How far into `.debug_line_str` one of the strings the table adds there starts, counted from
    /// the first of them.
    Text(usize),
}

/// The table, ready to be put in `.debug_line`, and the strings that go on the end of
/// `.debug_line_str` with it.
#[derive(Debug, Default)]
pub(crate) struct Table {
    pub(crate) bytes: Vec<u8>,
    pub(crate) holes: Vec<Hole>,
    pub(crate) strings: Vec<u8>,
}

/// Everything `.file` and `.loc` said, as the file is read.
#[derive(Debug, Default)]
pub(crate) struct Lines {
    /// Whether the file used slot zero of the file table, which only DWARF 5 has and which makes
    /// gas write a version five table. Version three otherwise, gas's default.
    five: bool,
    files: Vec<Option<File>>,
    dirs: Vec<Option<Vec<u8>>>,
    pub(crate) current: Loc,
    /// Whether a `.loc` is waiting for the next instruction to make its row.
    pub(crate) waiting: bool,
    /// Whether the file said `.loc` at all.
    pub(crate) seen: bool,
    /// The sections with rows in them, in the order their first rows were made.
    order: Vec<usize>,
    rows: Map<usize, Vec<Row>>,
    pieces: Map<usize, Pieces>,
}

impl Lines {
    /// `.file` with a number, which puts a file in a slot of the table.
    ///
    /// The directory goes in a table of its own, found or added there, and the file keeps the rest
    /// of its name. Slot zero is the file being compiled, and its directory goes in the table after
    /// the one it was compiled in, which is the first entry DWARF 5 wants there.
    pub(crate) fn file(
        &mut self,
        number: u64,
        dir: Option<Vec<u8>>,
        name: Vec<u8>,
        md5: Option<[u8; 16]>,
    ) -> Result<(), String> {
        if number == 0 {
            self.five = true;
        }
        let slot =
            usize::try_from(number).map_err(|_| format!("file number {number} is too big"))?;
        if slot >= 1 << 20 {
            return Err(format!("file number {number} is too big"));
        }
        if let Some(Some(had)) = self.files.get(slot).cloned() {
            return self.again(slot, &had, dir, &name, md5);
        }
        let (dirname, file0, base, cut) = if slot == 0 {
            let base = basename(&name);
            match dir {
                Some(dir) if base == 0 => {
                    let cut = dir.len();
                    (dir.clone(), Some(dir), 0, cut)
                }
                dir => (name.clone(), dir, base, base),
            }
        } else {
            match dir {
                None => {
                    let base = basename(&name);
                    (name.clone(), None, base, base)
                }
                Some(dir) => {
                    let cut = dir.len();
                    (dir, None, 0, cut)
                }
            }
        };
        let d = self.directory(&dirname, file0.as_deref(), cut, slot == 0);
        if self.files.len() <= slot {
            self.files.resize(slot + 1, None);
        }
        self.files[slot] =
            Some(File { name: name[base..].to_vec(), dir: d, md5: md5.unwrap_or_default() });
        Ok(())
    }

    /// A slot given a second time, which is fine when it is the same file and may fill in the
    /// directory the first time left out.
    fn again(
        &mut self,
        slot: usize,
        had: &File,
        dirname: Option<Vec<u8>>,
        name: &[u8],
        md5: Option<[u8; 16]>,
    ) -> Result<(), String> {
        let dir = self.dirs.get(had.dir).cloned().flatten();
        let clash = || {
            format!(
                "file table slot {slot} is already occupied by a different file ({}{}{} vs {}{}{})",
                dir.as_deref().map(String::from_utf8_lossy).unwrap_or_default(),
                if dir.is_some() { "/" } else { "" },
                String::from_utf8_lossy(&had.name),
                dirname.as_deref().map(String::from_utf8_lossy).unwrap_or_default(),
                if dirname.is_some() { "/" } else { "" },
                String::from_utf8_lossy(name),
            )
        };
        if md5.is_some_and(|md5| md5 != had.md5) {
            return Err(clash());
        }
        if let Some(given) = &dirname {
            if dir.as_ref().is_some_and(|dir| dir != given) || name != had.name.as_slice() {
                return Err(clash());
            }
            if dir.is_none() {
                self.set_dir(had.dir, given.clone());
            }
            return Ok(());
        }
        if let Some(dir) = &dir {
            let n = dir.len();
            if name.len() > n
                && name[..n] == dir[..]
                && name[n] == b'/'
                && name[n + 1..] == had.name
            {
                return Ok(());
            }
            return Err(clash());
        }
        let base = basename(name);
        if name[base..] != had.name {
            return Err(clash());
        }
        if base > 0 {
            self.set_dir(had.dir, name[..base].to_vec());
        }
        Ok(())
    }

    fn set_dir(&mut self, at: usize, dir: Vec<u8>) {
        if self.dirs.len() <= at {
            self.dirs.resize(at + 1, None);
        }
        self.dirs[at] = Some(dir);
    }

    /// The slot of the directory table that holds the first `len` bytes of `dirname`, adding it
    /// if it is not there. gas's `get_directory_table_entry`, including the way it compares the
    /// whole of `dirname` and not just the directory against the directory slot zero was compiled
    /// in.
    fn directory(&mut self, dirname: &[u8], file0: Option<&[u8]>, len: usize, zero: bool) -> usize {
        let mut len = len;
        if len == 0 {
            return 0;
        }
        if dirname[len - 1] == b'/' {
            len -= 1;
            if len == 0 {
                return 0;
            }
        }
        let wanted = &dirname[..len];
        if let Some(d) = self.dirs.iter().position(|dir| dir.as_deref() == Some(wanted)) {
            return d;
        }
        let mut d = self.dirs.len();
        if zero {
            if self.dirs.first().is_none_or(Option::is_none) {
                let pwd = file0.map_or_else(pwd, <[u8]>::to_vec);
                if self.five && dirname != pwd.as_slice() {
                    self.directory(&pwd, file0, pwd.len(), true);
                    d = 1;
                } else {
                    d = 0;
                }
            }
        } else if d == 0 {
            d = 1;
        }
        self.set_dir(d, wanted.to_vec());
        d
    }

    /// Where the next row goes, if one is due: the `.loc` before it is made a row here and its
    /// view worked out. The name the `.loc` gave the view comes back with the number, for the
    /// reader to set.
    ///
    /// A row in a section that holds no instructions is dropped, as gas drops it, and so is a row
    /// for line zero.
    pub(crate) fn row(&mut self, part: usize, at: u64, code: bool) -> Option<(String, u64)> {
        if !self.waiting {
            return None;
        }
        let loc = self.current.clone();
        self.consume();
        if loc.line == 0 || !code {
            return None;
        }
        let piece = self.pieces.entry(part).or_default();
        if piece.starts.is_empty() {
            piece.starts.push(0);
        }
        let home = (part, piece.starts.len() - 1, at);
        let rows = self.rows.entry(part).or_default();
        if rows.is_empty() {
            self.order.push(part);
        }
        let view = match rows.last() {
            Some(last) if !loc.reset => {
                if at > last.home.2 {
                    0
                } else {
                    last.view() + 1
                }
            }
            _ => 0,
        };
        let name = loc.view.clone();
        let mut row = Row { part, at, home, loc };
        row.loc.view = Some(view.to_string());
        rows.push(row);
        name.map(|name| (name, view))
    }

    /// What making a row uses up: the flags that are only about one row, and the view.
    fn consume(&mut self) {
        self.waiting = false;
        self.current.basic_block = false;
        self.current.prologue_end = false;
        self.current.epilogue_begin = false;
        self.current.discriminator = 0;
        self.current.view = None;
        self.current.reset = false;
    }

    /// Something of variable size in `part`, which gas puts in a piece of its own: the fixed part
    /// of the piece before it ends at `end`, and the next piece starts at `next`.
    ///
    /// The only thing that reads this is a `view -0` row, whose address gas sets again when the row
    /// before it is at the same place in the same piece, or, in a different piece, when the row is
    /// at the start of its piece and the row before it at the end of the fixed part of its own. That
    /// is the same address in the first case and may not be in the second, which is why the pieces
    /// have to be known at all. An alignment ends the fixed part where the padding starts, and a
    /// jump that may grow keeps its first byte in it, so a row in front of a jump is never at the end.
    pub(crate) fn variable(&mut self, part: usize, end: u64, next: u64) {
        let piece = self.pieces.entry(part).or_default();
        if piece.starts.is_empty() {
            piece.starts.push(0);
        }
        piece.ends.push(end);
        piece.starts.push(next);
    }

    /// Where a piece of a section starts.
    pub(crate) fn start(&self, part: usize, piece: usize) -> u64 {
        self.pieces.get(&part).and_then(|pieces| pieces.starts.get(piece)).copied().unwrap_or(0)
    }

    /// How many times a section has been cut so far, which numbers the piece the next byte goes in.
    pub(crate) fn cuts(&self, part: usize) -> usize {
        self.pieces.get(&part).map_or(0, |piece| piece.ends.len())
    }

    /// Every row, for the reader to move when it joins subsections on.
    pub(crate) fn rows_mut(&mut self) -> impl Iterator<Item = &mut Row> {
        self.rows.values_mut().flatten()
    }

    /// Whether a row has been made or slot one or zero been given, which is what decides there is
    /// a table to write.
    pub(crate) fn any_rows(&self) -> bool {
        !self.order.is_empty()
    }

    /// The table. `sections` is each section's rows' order and size: the sections that have rows,
    /// by where they ended up, in the order gas keeps them, which is the order the first row of
    /// each was made in, and how long each section is.
    pub(crate) fn table(&self, address: u8, size: impl Fn(usize) -> u64) -> Result<Table, String> {
        let version: u16 = if self.five { 5 } else { 3 };
        let mut table = Table::default();
        let out = &mut table.bytes;
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&version.to_le_bytes());
        if version >= 5 {
            out.extend_from_slice(&[address, 0]);
        }
        let length_at = out.len();
        out.extend_from_slice(&[0; 4]);
        let header = out.len();
        out.push(1);
        if version >= 4 {
            out.push(1);
        }
        out.push(1);
        out.push(LINE_BASE as u8);
        out.push(LINE_RANGE as u8);
        out.push(OPCODE_BASE as u8);
        out.extend_from_slice(&[0, 1, 1, 1, 1, 0, 0, 0, 1, 0, 0, 1]);
        if version >= 5 {
            self.five_lists(&mut table)?;
        } else {
            self.lists(&mut table.bytes)?;
        }
        let out = &mut table.bytes;
        let length = u32::try_from(out.len() - header).map_err(|_| "a line table too long")?;
        out[length_at..length_at + 4].copy_from_slice(&length.to_le_bytes());

        // The sections in the order their first rows were made, with the rows of a subsection
        // after those of the section, which is where joining them on put them.
        let mut sections: Vec<(usize, Vec<&Row>)> = Vec::new();
        for home in &self.order {
            let rows = &self.rows[home];
            let part = rows[0].part;
            match sections.iter_mut().find(|(at, _)| *at == part) {
                Some((_, all)) => all.extend(rows),
                None => sections.push((part, rows.iter().collect())),
            }
        }
        for (part, rows) in &mut sections {
            rows.sort_by_key(|row| row.at);
            self.program(&mut table, rows, address, size(*part));
        }
        let length = u32::try_from(table.bytes.len() - 4).map_err(|_| "a line table too long")?;
        table.bytes[..4].copy_from_slice(&length.to_le_bytes());
        Ok(table)
    }

    /// The directory and file tables of a version five header, whose names are offsets into
    /// `.debug_line_str`.
    fn five_lists(&self, table: &mut Table) -> Result<(), String> {
        let (dirs, files) = (self.dirs.len(), self.files.len());
        let out = &mut table.bytes;
        out.push(1);
        uleb(out, PATH);
        uleb(out, LINE_STRP);
        uleb(out, if dirs == 0 && files > 0 { 1 } else { dirs } as u64);
        if dirs > 0 || files > 0 {
            let first = self.dirs.first().cloned().flatten().unwrap_or_else(pwd);
            string(table, &first);
        }
        for dir in self.dirs.iter().skip(1) {
            string(table, dir.as_deref().unwrap_or_default());
        }
        // gas looks at the first byte of each sum to see whether there is one.
        let md5 = self.files.iter().flatten().any(|file| file.md5[0] != 0);
        let out = &mut table.bytes;
        out.push(2 + u8::from(md5));
        uleb(out, PATH);
        uleb(out, LINE_STRP);
        uleb(out, DIRECTORY);
        uleb(out, UDATA);
        if md5 {
            uleb(out, MD5);
            uleb(out, DATA16);
        }
        uleb(out, files as u64);
        // Slot zero left empty is filled from slot one, and then the two share one string.
        let mut shared = None;
        for (slot, file) in self.files.iter().enumerate() {
            let (file, copied) = match file {
                Some(file) => (file.clone(), false),
                None if slot == 0 => match self.files.get(1).cloned().flatten() {
                    Some(one) => {
                        let md5 = if md5 { one.md5 } else { [0; 16] };
                        (File { md5, ..one }, true)
                    }
                    None => (File { name: Vec::new(), dir: 0, md5: [0; 16] }, false),
                },
                None => return Err(format!("unassigned file number {slot}")),
            };
            match shared.take() {
                Some(at) => hole(table, To::Text(at), 4),
                None => {
                    let at = string(table, &file.name);
                    if copied {
                        shared = Some(at);
                    }
                }
            }
            uleb(&mut table.bytes, file.dir as u64);
            if md5 {
                table.bytes.extend_from_slice(&file.md5);
            }
        }
        Ok(())
    }

    /// The directory and file tables of a version three header, as strings in the header itself.
    fn lists(&self, out: &mut Vec<u8>) -> Result<(), String> {
        for dir in self.dirs.iter().skip(1) {
            out.extend_from_slice(dir.as_deref().unwrap_or_default());
            out.push(0);
        }
        out.push(0);
        for (slot, file) in self.files.iter().enumerate().skip(1) {
            let Some(file) = file else {
                return Err(format!("unassigned file number {slot}"));
            };
            out.extend_from_slice(&file.name);
            out.push(0);
            uleb(out, file.dir as u64);
            out.extend_from_slice(&[0, 0]);
        }
        out.push(0);
        Ok(())
    }

    /// One section's rows, from the first address to the end of the section.
    fn program(&self, table: &mut Table, rows: &[&Row], address: u8, size: u64) {
        let mut file = 1;
        let mut line = 1i64;
        let mut column = 0;
        let mut isa = 0;
        let mut stmt = true;
        let mut before: Option<&Row> = None;
        for row in rows {
            let loc = &row.loc;
            let out = &mut table.bytes;
            if file != loc.file {
                file = loc.file;
                out.push(SET_FILE);
                uleb(out, file);
            }
            if column != loc.column {
                column = loc.column;
                out.push(SET_COLUMN);
                uleb(out, column);
            }
            if loc.discriminator != 0 {
                let mut number = Vec::new();
                uleb(&mut number, loc.discriminator);
                out.push(0);
                sleb(out, 1 + number.len() as i64);
                out.push(SET_DISCRIMINATOR);
                out.extend_from_slice(&number);
            }
            if isa != loc.isa {
                isa = loc.isa;
                out.push(SET_ISA);
                uleb(out, isa);
            }
            if stmt != loc.stmt {
                stmt = loc.stmt;
                out.push(NEGATE_STMT);
            }
            if loc.basic_block {
                out.push(SET_BASIC_BLOCK);
            }
            if loc.prologue_end {
                out.push(SET_PROLOGUE_END);
            }
            if loc.epilogue_begin {
                out.push(SET_EPILOGUE_BEGIN);
            }
            let delta = loc.line as i64 - line;
            match before {
                Some(last) if !(loc.reset && self.set_again(last, row)) => {
                    advance(out, Some(delta), row.at - last.at);
                }
                _ => {
                    out.push(0);
                    uleb(out, u64::from(address) + 1);
                    out.push(SET_ADDRESS);
                    hole(table, To::Code { part: row.part, at: row.at }, address);
                    advance(&mut table.bytes, Some(delta), 0);
                }
            }
            line = loc.line as i64;
            before = Some(row);
        }
        if let Some(last) = before {
            advance(&mut table.bytes, None, size.saturating_sub(last.at));
        }
    }

    /// Whether gas sets the address again for a `view -0` row after `last`. See [`Lines::variable`].
    fn set_again(&self, last: &Row, row: &Row) -> bool {
        let (part, piece, at) = row.home;
        let (was_part, was_piece, was_at) = last.home;
        if (part, piece) == (was_part, was_piece) {
            return at == was_at;
        }
        let Some(pieces) = self.pieces.get(&part) else { return false };
        let starts = pieces.starts.get(piece) == Some(&at);
        let ended = self
            .pieces
            .get(&was_part)
            .and_then(|pieces| pieces.ends.get(was_piece))
            .is_some_and(|&end| was_at >= end);
        starts && ended
    }
}

impl Row {
    fn view(&self) -> u64 {
        self.loc.view.as_deref().and_then(|view| view.parse().ok()).unwrap_or(0)
    }
}

/// A line and an address moved on together, in as few bytes as gas finds for it. `None` for the
/// line is the end of the sequence, which moves the address and makes no row of its own.
fn advance(out: &mut Vec<u8>, line: Option<i64>, address: u64) {
    let Some(mut line) = line else {
        if address == MOST_SPECIAL {
            out.push(CONST_ADD_PC);
        } else if address != 0 {
            out.push(ADVANCE_PC);
            uleb(out, address);
        }
        out.extend_from_slice(&[0, 1, END_SEQUENCE]);
        return;
    };
    let mut copy = false;
    if !(0..LINE_RANGE as i64).contains(&(line - LINE_BASE)) {
        out.push(ADVANCE_LINE);
        sleb(out, line);
        line = 0;
        copy = true;
    }
    if line == 0 && address == 0 {
        out.push(COPY);
        return;
    }
    let base = (line - LINE_BASE) as u64 + OPCODE_BASE;
    if address < 256 + MOST_SPECIAL {
        let op = base + address * LINE_RANGE;
        if op <= 255 {
            out.push(op as u8);
            return;
        }
        if address >= MOST_SPECIAL {
            let op = base + (address - MOST_SPECIAL) * LINE_RANGE;
            if op <= 255 {
                out.extend_from_slice(&[CONST_ADD_PC, op as u8]);
                return;
            }
        }
    }
    out.push(ADVANCE_PC);
    uleb(out, address);
    out.push(if copy { COPY } else { base as u8 });
}

/// A string added to `.debug_line_str` and a hole in the table that points at it, which comes
/// back as where the string starts.
fn string(table: &mut Table, text: &[u8]) -> usize {
    let at = table.strings.len();
    table.strings.extend_from_slice(text);
    table.strings.push(0);
    hole(table, To::Text(at), 4);
    at
}

fn hole(table: &mut Table, to: To, width: u8) {
    table.holes.push(Hole { at: table.bytes.len(), width, to });
    table.bytes.extend(std::iter::repeat_n(0, usize::from(width)));
}

/// Where the last part of a path starts, which is after its last `/`, except that a path whose
/// only `/` is the first byte is taken whole, as gas takes it.
fn basename(path: &[u8]) -> usize {
    match path.iter().rposition(|&byte| byte == b'/') {
        Some(0) | None => 0,
        Some(at) => at + 1,
    }
}

/// The directory the assembler is running in, which gas puts first in the directory table when
/// the file did not say which one it was compiled in.
fn pwd() -> Vec<u8> {
    std::env::current_dir()
        .map(|dir| dir.to_string_lossy().into_owned().into_bytes())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(line: Option<i64>, address: u64) -> Vec<u8> {
        let mut out = Vec::new();
        advance(&mut out, line, address);
        out
    }

    #[test]
    fn a_line_and_an_address_are_one_special_opcode_where_one_reaches() {
        assert_eq!(bytes(Some(1), 0), [0x13]);
        assert_eq!(bytes(Some(0), 0), [COPY]);
        assert_eq!(bytes(Some(1), 3), [0x13 + 3 * 14]);
        assert_eq!(bytes(Some(0), 20), [CONST_ADD_PC, 0x12 + 3 * 14]);
        assert_eq!(bytes(Some(-6), 0), [ADVANCE_LINE, 0x7a, COPY]);
        assert_eq!(bytes(Some(20), 2), [ADVANCE_LINE, 20, 0x12 + 2 * 14]);
        assert_eq!(bytes(Some(0), 300), [ADVANCE_PC, 0xac, 0x02, 0x12]);
        assert_eq!(bytes(None, MOST_SPECIAL), [CONST_ADD_PC, 0, 1, 1]);
        assert_eq!(bytes(None, 3), [ADVANCE_PC, 3, 0, 1, 1]);
    }

    #[test]
    fn slot_zero_puts_the_compile_directory_first_and_its_own_after() {
        let mut lines = Lines::default();
        lines.file(0, Some(b"/w/out".to_vec()), b"/src/boot/a.c".to_vec(), None).unwrap();
        lines.file(1, None, b"/src/boot/a.c".to_vec(), None).unwrap();
        lines.file(2, None, b"/src/include/b.h".to_vec(), None).unwrap();
        lines.file(3, None, b"c.h".to_vec(), None).unwrap();
        let dirs: Vec<_> = lines.dirs.iter().flatten().map(|d| d.as_slice()).collect();
        assert_eq!(dirs, [&b"/w/out"[..], b"/src/boot", b"/src/include"]);
        let files: Vec<_> =
            lines.files.iter().flatten().map(|f| (f.name.as_slice(), f.dir)).collect();
        assert_eq!(files, [(&b"a.c"[..], 1), (b"a.c", 1), (b"b.h", 2), (b"c.h", 0)]);
        assert!(lines.file(1, None, b"/src/boot/a.c".to_vec(), None).is_ok());
        assert!(lines.file(1, None, b"/src/boot/z.c".to_vec(), None).is_err());
    }

    #[test]
    fn a_view_counts_rows_at_one_address_and_minus_zero_starts_again() {
        let mut lines = Lines::default();
        let at = |lines: &mut Lines, place: u64, name: &str, reset: bool| {
            lines.current.view = Some(name.to_owned());
            lines.current.reset = reset;
            lines.waiting = true;
            lines.row(0, place, true).map(|(_, view)| view)
        };
        assert_eq!(at(&mut lines, 0, "a", true), Some(0));
        assert_eq!(at(&mut lines, 0, "b", false), Some(1));
        assert_eq!(at(&mut lines, 0, "c", false), Some(2));
        assert_eq!(at(&mut lines, 4, "d", false), Some(0));
        assert_eq!(at(&mut lines, 4, "e", true), Some(0));
        assert_eq!(at(&mut lines, 4, "f", false), Some(1));
        lines.waiting = true;
        assert_eq!(lines.row(1, 0, false), None, "no row outside code");
    }
}
