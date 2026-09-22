//! The line table, as the bytes of the sections it goes in.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.4.
//!
//! A line table answers one question: given an address in the program, which line of which file was
//! the compiler writing code for when it produced the instruction there. Everything else DWARF
//! describes is about what the program means, and this is about where it came from, which is why it
//! is a table of its own rather than an attribute on something.
//!
//! # Why this is the first part written
//!
//! Because of what a safety report is. The monitor's descriptor carries a judgement, a class, an
//! access size and a program counter, and `spec/safe-memory/06-instrumentation.md` section 6.5
//! deliberately keeps the source location out of it so that a compiler does not ship two line
//! tables that can come to disagree. That is the right design and it only pays when there is one
//! line table, and until now there was none, so every report from a corpus run had to be read
//! backwards out of a disassembly. On a quarter of a million lines of SQLite that is the difference
//! between a minute and an afternoon per shape.
//!
//! # What is here
//!
//! The line number program, in `.debug_line`, with the file and directory tables DWARF 5 puts in
//! its header, and the strings those tables name, in `.debug_line_str`. Beside them the smallest
//! compilation unit that makes them findable: one `DW_TAG_compile_unit` in `.debug_info` with the
//! producer, the name of the file, the directory the compiler ran in, a `DW_AT_stmt_list` pointing
//! at the program and a `DW_AT_ranges` saying which addresses this unit covers, and the
//! abbreviation it is written against in `.debug_abbrev`. A reader handed an address walks the
//! units, and a unit with no entry in `.debug_info` is a unit nothing walks, so the table alone
//! would have been a section no tool reads.
//!
//! The ranges are a list with one entry per function rather than a low and a high address over the
//! whole unit. Under `-ffunction-sections` each function is a section of its own and the linker may
//! place them anywhere and drop the ones nothing reaches, so there is no single span that covers
//! them, and writing one would be writing down something that is true of the object and false of
//! the program.
//!
//! # What is not here
//!
//! What the program means is in `tree.rs` and goes in the same unit: the types, the functions and
//! the variables the unit defines at file scope, which is what a debugger reads a value through.
//! Half the locals are there too, which is the ones lowering gave a frame slot, each a
//! `DW_OP_fbreg` at an offset the frame layout worked out. The other half are held in SSA values
//! and need a location list built over the register allocator's output, which is the rest of
//! tamnd/rucc#9. What makes a location a different piece of work rather than more of this one is
//! that it is checked differently: a line table is right or wrong against `addr2line` and a local's
//! location is right or wrong against a debugger that stops in the middle of a function and prints
//! it.
//!
//! One sequence per function, each beginning at that function's own symbol. A sequence is the unit
//! of address ordering in a line program and its rows have to run forwards, so a table with one
//! sequence over a section would be a table that breaks the moment two functions are laid out in an
//! order the source did not have. One per function costs a `DW_LNE_set_address` and a relocation
//! each and is correct under every combination of flags there is.

use crate::shape::{Global, Local, Place, Scope, Shape, Sig};
use crate::tree;

use rucc_object::{Chunk, Info, Reference, Reloc};

/// One compilation unit's worth of debug information.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Unit {
    /// The file being compiled, as the command line spelled it, already prefix mapped.
    pub name: String,
    /// The directory the compiler was run in, already prefix mapped.
    ///
    /// This is `DW_AT_comp_dir`, and what it is for is that every relative name in the tables below
    /// is relative to it. A build that cannot say where it ran writes a single dot, which is what
    /// the tables are already relative to and is therefore the one answer that changes nothing.
    pub dir: String,
    /// What produced this, which is this compiler and its version.
    pub producer: String,
    /// Every file any row names, in the order the rows refer to them by.
    pub files: Vec<String>,
    /// Every type anything in the unit names, in the order they refer to them by.
    ///
    /// A table of indices rather than a tree, so that a type naming itself is an ordinary entry.
    /// See [`Shape`] for what is in one and what is deliberately left out of one.
    pub types: Vec<Shape>,
    /// The functions, in the order the text section holds them.
    pub funcs: Vec<Function>,
    /// The file-scope variables this unit defines, in the order the object file holds them.
    ///
    /// Only the ones it defines. A name this unit declares and another one defines is a name the
    /// linker resolves, so an entry for it here would be an entry whose address is somebody
    /// else's, and a reader wanting the type of one reads the unit that has it.
    pub globals: Vec<Global>,
    /// How many bytes an address is on this target.
    pub pointer: u8,
    /// Whether this build writes a call frame table, which is what a frame base is resolved
    /// through.
    ///
    /// A function's `DW_AT_frame_base` is `DW_OP_call_frame_cfa`, and what answers that operation
    /// is the unwind table the build already writes for every function. A build that turns the
    /// table off, which is a kernel or a freestanding image, leaves a reader with nothing to
    /// evaluate the operation against, so the attribute is left off there rather than written as
    /// something no debugger can follow. The locations that would be measured from it are left off
    /// with it.
    pub frames: bool,
}

/// One function: where each of its instructions came from, and what it is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Function {
    /// Its name, as the C program spelled it, which is what a relocation here asks the linker for.
    pub name: String,
    /// How many bytes of instructions it is.
    pub len: u64,
    /// The rows, in increasing order of address.
    pub rows: Vec<Row>,
    /// Where it was declared, and nothing when that is not known.
    pub decl: Option<Place>,
    /// What it takes and gives back, and [`None`] when this compiler cannot yet say.
    ///
    /// A function with nothing here gets no entry in `.debug_info` at all, for the reason in the
    /// `tree.rs` module documentation: an entry with no return type is an entry saying `void`, so
    /// half an answer here is a wrong one rather than a partial one.
    pub sig: Option<Sig>,
    /// Whether anything outside this unit can see it, which is the opposite of `static`.
    pub external: bool,
    /// The locals lowering gave a frame slot, in the order the slots were asked for, which is the
    /// order they were declared in.
    ///
    /// Parameters are not among them, whether or not they have a slot. See [`Local`].
    pub locals: Vec<Local>,
    /// The inner scopes of the function, each after the scope it is written inside.
    ///
    /// The function's own body is not one of them, for the reason [`Scope`] gives. A scope nothing
    /// above names is written down anyway and costs nothing: an entry is only made for one that has
    /// a local of its own or holds a scope that does.
    pub scopes: Vec<Scope>,
}

/// One row of the table: an address, and where the code at it came from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Row {
    /// How far into its function the instruction is.
    pub at: u64,
    /// Which of [`Unit::files`] it is in.
    pub file: usize,
    /// Which line of that file, counting from one, or zero for code no line of any file asked for.
    ///
    /// Zero is DWARF's own spelling of that and is worth more than a guess: a debugger stepping
    /// over a row with no line knows not to stop, where one handed the nearest line it could find
    /// would stop somewhere the program never was.
    pub line: u32,
    /// Which column of that line, counting from one, or zero for the left edge.
    pub column: u32,
}

/// What went wrong while the sections were being built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The DWARF writer refused something, which is a bug here rather than in a program.
    Refused {
        /// What it said, already formatted.
        why: String,
    },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Refused { why } => {
                write!(f, "the debug writer refused what it was given: {why}")
            }
        }
    }
}

impl std::error::Error for Error {}

/// One section of DWARF being written, and the relocations found while writing it.
///
/// The writer underneath hands a relocation over the moment it writes the hole rather than at the
/// end, because the hole's offset is the length of what it has written so far, so this has to be
/// one type rather than bytes now and relocations later.
#[derive(Debug, Clone)]
struct Section {
    bytes: gimli::write::EndianVec<gimli::LittleEndian>,
    relocs: Vec<gimli::write::Relocation>,
}

impl Default for Section {
    fn default() -> Self {
        Self { bytes: gimli::write::EndianVec::new(gimli::LittleEndian), relocs: Vec::new() }
    }
}

impl gimli::write::RelocateWriter for Section {
    type Writer = gimli::write::EndianVec<gimli::LittleEndian>;

    fn writer(&self) -> &Self::Writer {
        &self.bytes
    }

    fn writer_mut(&mut self) -> &mut Self::Writer {
        &mut self.bytes
    }

    fn relocate(&mut self, relocation: gimli::write::Relocation) {
        self.relocs.push(relocation);
    }
}

/// The sections a unit's debug information goes in.
///
/// The result is empty when nothing in the unit has a row, which is a file of declarations and a
/// file whose every function was dropped. An empty `.debug_line` is worse than no section at all,
/// since a reader would find a unit covering no addresses and have to decide what that meant.
///
/// # Errors
///
/// [`Error::Refused`] for anything the DWARF writer objected to. Every value it is handed here came
/// out of this compiler, so that is a bug here rather than a program's mistake.
pub fn write(unit: &Unit) -> Result<Info, Error> {
    if unit.funcs.iter().all(|func| func.rows.is_empty()) {
        return Ok(Info::default());
    }
    let encoding =
        gimli::Encoding { format: gimli::Format::Dwarf32, version: 5, address_size: unit.pointer };
    let mut dwarf = gimli::write::DwarfUnit::new(encoding);
    let dir = text(&unit.dir, encoding, &mut dwarf.line_strings);
    let name = text(&unit.name, encoding, &mut dwarf.line_strings);
    let mut program =
        gimli::write::LineProgram::new(encoding, gimli::LineEncoding::default(), dir, name, None);
    // Every file the rows name, under the directory above. The name is written whole rather than
    // split into a directory and a base, which is legal and is not what gcc does: a reader joins
    // the two only when the file is relative, so a whole name is one the reader takes as it stands.
    // Splitting would buy a shorter table on a project whose files share directories and would put
    // a second place in here where a path is taken apart.
    let under = program.default_directory();
    let files: Vec<gimli::write::FileId> = unit
        .files
        .iter()
        .map(|file| {
            let file = text(file, encoding, &mut dwarf.line_strings);
            program.add_file(file, under, None)
        })
        .collect();
    for (index, func) in unit.funcs.iter().enumerate() {
        if func.rows.is_empty() {
            continue;
        }
        program.begin_sequence(Some(gimli::write::Address::Symbol { symbol: index, addend: 0 }));
        // A row that says what the row before it said is a row a reader would read and discard, so
        // it is left out. That is most of them: a line of C is several instructions and every one
        // of them carries the same span.
        let mut said: Option<(usize, u32, u32)> = None;
        for row in &func.rows {
            let now = (row.file, row.line, row.column);
            if said == Some(now) {
                continue;
            }
            said = Some(now);
            let Some(&file) = files.get(row.file) else {
                let why = format!("row at {} names file {}, which is not one", row.at, row.file);
                return Err(Error::Refused { why });
            };
            let state = program.row();
            state.address_offset = row.at;
            state.file = file;
            state.line = u64::from(row.line);
            state.column = u64::from(row.column);
            // Every row is somewhere a breakpoint may attach, because at this optimization level
            // every row is the start of a statement or is code with no statement to be the start
            // of, and the second kind carries line zero and is not a place a debugger stops.
            state.is_statement = true;
            program.generate_row();
        }
        program.end_sequence(func.len);
    }
    let ranges = unit
        .funcs
        .iter()
        .enumerate()
        .filter(|(_, func)| !func.rows.is_empty())
        .map(|(index, func)| gimli::write::Range::StartLength {
            begin: gimli::write::Address::Symbol { symbol: index, addend: 0 },
            length: func.len,
        })
        .collect();
    dwarf.unit.line_program = program;
    let covers = dwarf.unit.ranges.add(gimli::write::RangeList(ranges));
    let root = dwarf.unit.root();
    let producer = text(&unit.producer, encoding, &mut dwarf.line_strings);
    let name = text(&unit.name, encoding, &mut dwarf.line_strings);
    let dir = text(&unit.dir, encoding, &mut dwarf.line_strings);
    let root = dwarf.unit.get_mut(root);
    root.set(gimli::DW_AT_producer, gimli::write::AttributeValue::LineStringRef(held(producer)?));
    root.set(gimli::DW_AT_language, gimli::write::AttributeValue::Language(gimli::DW_LANG_C11));
    root.set(gimli::DW_AT_name, gimli::write::AttributeValue::LineStringRef(held(name)?));
    root.set(gimli::DW_AT_comp_dir, gimli::write::AttributeValue::LineStringRef(held(dir)?));
    root.set(gimli::DW_AT_stmt_list, gimli::write::AttributeValue::LineProgramRef);
    root.set(gimli::DW_AT_ranges, gimli::write::AttributeValue::RangeListRef(covers));
    tree::describe(&mut dwarf, &unit.types, &files, &unit.funcs, &unit.globals, unit.frames)?;
    let mut sections = gimli::write::Sections::new(Section::default());
    dwarf.write(&mut sections).map_err(refused)?;
    let mut info = Info::default();
    // One index space over both lists, the functions first. `gimli` calls a relocation target a
    // symbol number and leaves it to the caller to say what a number means, and what one means here
    // is a position in this: the line table and a subprogram's low PC ask for a function, and a
    // variable's location asks for a variable.
    let named = |target: gimli::write::RelocationTarget| match target {
        gimli::write::RelocationTarget::Symbol(index) => match unit.funcs.get(index) {
            Some(func) => func.name.clone(),
            None => unit.globals[index - unit.funcs.len()].name.clone(),
        },
        gimli::write::RelocationTarget::Section(id) => id.name().to_owned(),
    };
    sections.for_each(|id, section| {
        if section.bytes.slice().is_empty() {
            return Ok(());
        }
        let relocs = section
            .relocs
            .iter()
            .map(|reloc| Reloc {
                at: reloc.offset,
                symbol: named(reloc.target),
                kind: Reference::Address { bytes: reloc.size },
                addend: reloc.addend,
                after: 0,
            })
            .collect();
        info.chunks.push(Chunk {
            name: id.name().to_owned(),
            bytes: section.bytes.slice().to_vec(),
            relocs,
        });
        Ok::<(), Error>(())
    })?;
    Ok(info)
}

/// A string as the line program writes one, which is a reference into `.debug_line_str`.
///
/// Every string here goes in that section rather than in `.debug_str` or inline, because the file
/// and directory tables of a DWARF 5 line program can reach it and the unit's own attributes can
/// too, so one section holds all of them and a name that appears in both is written once.
fn text(
    val: &str,
    encoding: gimli::Encoding,
    strings: &mut gimli::write::LineStringTable,
) -> gimli::write::LineString {
    // A null byte in a path is not something a file system hands back and is something the writer
    // underneath panics on, so it is taken out rather than passed through.
    let val: Vec<u8> = val.bytes().filter(|&byte| byte != 0).collect();
    gimli::write::LineString::new(val, encoding, strings)
}

/// The identifier behind a string that went into `.debug_line_str`.
///
/// [`text`] answers with whichever form of string the encoding wanted, and for DWARF 5 that is
/// always a reference into that section. An attribute has to name the reference rather than repeat
/// the bytes, so this is where the one shape the encoding can produce is taken apart, and anything
/// else is a disagreement between this function and that one rather than anything a caller did.
fn held(string: gimli::write::LineString) -> Result<gimli::write::LineStringId, Error> {
    match string {
        gimli::write::LineString::LineStringRef(id) => Ok(id),
        _ => Err(Error::Refused {
            why: "a string meant for the line string section was written another way".to_owned(),
        }),
    }
}

/// What the DWARF writer said, as the one kind of news it can be here.
fn refused(why: gimli::write::Error) -> Error {
    Error::Refused { why: why.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unit with one function and two lines in it.
    fn one() -> Unit {
        Unit {
            name: "a.c".to_owned(),
            dir: "/tmp".to_owned(),
            producer: "rucc".to_owned(),
            files: vec!["a.c".to_owned()],
            types: Vec::new(),
            funcs: vec![Function {
                name: "f".to_owned(),
                len: 16,
                rows: vec![
                    Row { at: 0, file: 0, line: 3, column: 1 },
                    Row { at: 8, file: 0, line: 4, column: 5 },
                ],
                ..Function::default()
            }],
            globals: Vec::new(),
            pointer: 8,
            frames: true,
        }
    }

    /// The sections that come out, and that each of them has something in it.
    ///
    /// Four rather than two, because a line table nothing can find is a section no reader opens.
    /// The unit in `.debug_info` is what a reader walks to reach the program, the abbreviation in
    /// `.debug_abbrev` is what that unit is written against, and the strings are in
    /// `.debug_line_str` because both the unit and the program's own tables name them.
    #[test]
    fn a_unit_with_rows_writes_the_four_sections_a_reader_needs() {
        let info = write(&one()).expect("sections");
        let names: Vec<&str> = info.chunks.iter().map(|chunk| chunk.name.as_str()).collect();
        assert_eq!(
            names,
            [".debug_abbrev", ".debug_line_str", ".debug_line", ".debug_rnglists", ".debug_info"]
        );
        assert!(info.chunks.iter().all(|chunk| !chunk.bytes.is_empty()));
    }

    /// Where a function is is the one number no compilation knows, so every sequence asks for it.
    ///
    /// The relocation names the function rather than the section it is in, because under
    /// `-ffunction-sections` the section is the function's own and under anything else the object
    /// writer is the one that knows where in the text it landed. The others in the same section are
    /// the header naming its own strings, which is the other thing only a linker can resolve.
    #[test]
    fn a_sequence_asks_the_linker_where_its_function_went() {
        let info = write(&one()).expect("sections");
        let line = info.chunks.iter().find(|chunk| chunk.name == ".debug_line").expect("a table");
        let address = line.relocs.iter().find(|reloc| reloc.symbol == "f").expect("an address");
        assert_eq!(address.kind, Reference::Address { bytes: 8 });
        assert_eq!(address.addend, 0);
        // The rest are the header's own, and they are section offsets rather than addresses: a
        // directory and a file name in DWARF 5 are written as a place in `.debug_line_str`.
        let rest = line.relocs.iter().filter(|reloc| reloc.symbol != "f");
        assert!(rest.clone().count() > 0);
        assert!(rest.clone().all(|reloc| reloc.symbol == ".debug_line_str"));
        assert!(rest.clone().all(|reloc| reloc.kind == Reference::Address { bytes: 4 }));
    }

    /// A file with nothing to say writes no sections rather than empty ones.
    #[test]
    fn a_unit_with_no_rows_writes_nothing() {
        let mut unit = one();
        unit.funcs[0].rows.clear();
        assert_eq!(write(&unit).expect("sections"), Info::default());
    }

    /// A row naming a file the unit does not have is refused rather than written as something else.
    #[test]
    fn a_row_naming_a_file_that_is_not_there_is_refused() {
        let mut unit = one();
        unit.funcs[0].rows[1].file = 7;
        assert!(write(&unit).is_err());
    }
}
