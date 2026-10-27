//! The debug information gas makes up for a file of assembly under `-g`.
//!
//! gcc hands `-g` on to gas as `--gdwarf-5` when the input is assembly, and gas then writes what a
//! debugger needs to step through the file: a line table with a row for each source line that laid
//! down an instruction, and a unit in `.debug_info` that names the file, the directory and every
//! function `.type` and `.size` described. The kernel's build asks for it for every `.S`, and
//! pahole turns those functions into the BTF the kernel checks its own against, so a function
//! written in assembly that this left out is a hole in the BTF a gcc kernel does not have.
//!
//! This is that, done the way gas 2.42 does it in `dwarf2dbg.c`.
//!
//! # Where a row says it is
//!
//! At the line the preprocessor's markers say it is on, `# 12 "file.h" 1`, so a row for an
//! instruction in a header names the header. A row is made for the first instruction of a line and
//! not again until the line or the file changes, wherever the instructions in between went. Data,
//! padding and labels make none, and an instruction in a section that is not code makes none
//! either, though it still counts as the line the last row was for.
//!
//! The whole expansion of a macro is on the line that called it, and so is a repetition inside one.
//! A repetition outside one has each statement of its body on the line it was written on, each time
//! round.
//!
//! The files are numbered as the table is written, section by section, rather than as the rows are
//! made. See [`crate::lines::Lines::number`].
//!
//! # What else is written
//!
//! `.debug_info` holds one unit: the line table, the code it covers, the name of the first file in
//! the table, the directory and `GNU AS` with the version. Under it is a `DW_TAG_subprogram` for
//! each name that is a function, is defined and has a size, in the order the names were first
//! written, with its address, its size and whether it is global, and a `DW_TAG_unspecified_type`
//! they all return. `.debug_abbrev` describes those, `.debug_aranges` lists the code, and when the
//! code is in more than one section `.debug_rnglists` lists the sections, or `.debug_ranges` under
//! DWARF 4. The names are in `.debug_str`.
//!
//! A file that says `.file` with a number or `.loc` is describing its own lines, and gas throws away
//! the rows it made and writes the table from what the file said. That is what happens here too.

use rucc_object::{Binding, Held, Reference, Shape, Sort};

use super::{Fixup, Reach, Reader, Sum, Term, Trouble, What};
use crate::lines::Loc;
use crate::unwind::uleb;

/// The sections the debug information goes in, made by the line table in the order gas makes them
/// and filled once every size is known.
#[derive(Debug)]
pub(super) struct Made {
    /// `.debug_line`, and where the table starts in it.
    line: (usize, u64),
    info: usize,
    abbrev: usize,
    aranges: usize,
    strings: usize,
    /// `.debug_rnglists` or `.debug_ranges`, when the code is in more than one section.
    ranges: Option<usize>,
    /// The sections that have rows, in the order the table lists them.
    code: Vec<usize>,
}

/// Where a hole in one of the sections points.
enum To {
    /// A place in a section, which is written as the section and an offset.
    Place(usize, u64),
    /// A name, which is written as the name when it is global and as its section otherwise.
    Name(String),
}

/// The bytes of one section, with the holes in them the linker fills.
#[derive(Default)]
struct Bytes {
    out: Vec<u8>,
    holes: Vec<(usize, u8, To)>,
}

impl Bytes {
    fn hole(&mut self, width: u8, to: To) {
        self.holes.push((self.out.len(), width, to));
        self.out.extend(std::iter::repeat_n(0, usize::from(width)));
    }

    /// A number `width` bytes wide.
    fn number(&mut self, width: u8, value: u64) {
        self.out.extend_from_slice(&value.to_le_bytes()[..usize::from(width)]);
    }

    /// The length the unit starts with, which counts everything after itself.
    fn length(&mut self) {
        let length = (self.out.len() - 4) as u32;
        self.out[..4].copy_from_slice(&length.to_le_bytes());
    }
}

/// How `DW_AT_external` is written, which gas decides from the functions there are.
#[derive(Clone, Copy, PartialEq, Eq)]
enum External {
    /// Not at all, when no function is global.
    None,
    /// `DW_FORM_flag_present`, when every one is.
    Present,
    /// `DW_FORM_flag`, a byte for each, when some are and some are not.
    Flag,
}

/// One function the unit names.
struct Function {
    name: String,
    global: bool,
    size: u64,
}

/// `DW_TAG_compile_unit`, `DW_TAG_subprogram` and `DW_TAG_unspecified_type`.
const COMPILE_UNIT: u8 = 0x11;
const SUBPROGRAM: u8 = 0x2e;
const UNSPECIFIED_TYPE: u8 = 0x3b;
/// `DW_LANG_Mips_Assembler`, which is what gas says the language is.
const ASSEMBLER: u16 = 0x8001;

impl Reader {
    /// What has to be in place before the first line is read under `-g`: the version of the table,
    /// the directory it names first and the file the lines are in until a marker says otherwise.
    pub(super) fn start_debug(&mut self) {
        let Some(debug) = &self.debug else { return };
        let (version, dir, name) = (debug.version, debug.dir.clone(), debug.name.clone());
        self.lines.version = Some(u16::from(version));
        self.lines.home = Some(self.remap(&dir));
        let name = self.source(&name);
        self.next = (name, 1);
    }

    /// A path with the last rewrite of `-fdebug-prefix-map=` that matches it applied.
    fn remap(&self, path: &str) -> Vec<u8> {
        let remap = self.debug.as_ref().map(|debug| debug.remap.as_slice()).unwrap_or_default();
        for (old, new) in remap.iter().rev() {
            if let Some(rest) = path.strip_prefix(old.as_str()) {
                return format!("{new}{rest}").into_bytes();
            }
        }
        path.as_bytes().to_vec()
    }

    /// Where a file the lines came from is in [`Reader::sources`], adding it if it is new.
    fn source(&mut self, name: &str) -> usize {
        let name = self.remap(name);
        if let Some(at) = self.sources.iter().position(|had| *had == name) {
            return at;
        }
        self.sources.push(name);
        self.sources.len() - 1
    }

    /// One line of the text, as a place in the source: a marker the preprocessor left moves the
    /// lines after it, and any other line is the next line of the file it is in.
    pub(super) fn position(&mut self, raw: &str) {
        if let Some((line, file)) = marker(raw) {
            self.next = (self.source(&file), line);
            return;
        }
        self.place = self.next;
        self.next.1 += 1;
    }

    /// The row an instruction makes under `-g`, which [`Reader::row`] then puts down where the
    /// instruction goes.
    pub(super) fn generated(&mut self) {
        if !self.generating {
            return;
        }
        let (file, line) = self.place;
        if line == 0 || self.last_place == Some(self.place) {
            return;
        }
        self.last_place = Some(self.place);
        self.lines.current = Loc { file: file as u64, line, ..Loc::default() };
        self.lines.waiting = true;
    }

    /// `.file` with a number or `.loc`, which ends the rows made from the source lines and throws
    /// away the ones made already.
    pub(super) fn stop_generating(&mut self) {
        if self.generating {
            self.generating = false;
            self.lines.purge();
        }
    }

    /// The sections after `.debug_line`, made empty in the order gas makes them so that they come
    /// out in that order, to be filled by [`Reader::debug_sections`].
    pub(super) fn make_debug_sections(&mut self, line: usize, start: u64) {
        let (here, before) = (self.here, self.before);
        let five = self.debug.as_ref().is_some_and(|debug| debug.version >= 5);
        let code = self.lines.parts();
        let plain = Shape { bits: true, ..Shape::default() };
        let make = |reader: &mut Reader, name: &str, shape: Shape| {
            reader.section(name, shape);
            reader.here
        };
        let info = make(self, ".debug_info", plain);
        let abbrev = make(self, ".debug_abbrev", plain);
        let aranges = make(self, ".debug_aranges", plain);
        let strings = make(self, ".debug_str", Shape { merge: 1, strings: true, ..plain });
        let ranges = (code.len() > 1)
            .then(|| make(self, if five { ".debug_rnglists" } else { ".debug_ranges" }, plain));
        (self.here, self.before) = (here, before);
        self.made =
            Some(Made { line: (line, start), info, abbrev, aranges, strings, ranges, code });
    }

    /// The unit, its abbreviations, the list of the code and the names, now that every function
    /// has its size.
    pub(super) fn debug_sections(&mut self) -> Result<(), Trouble> {
        let Some(made) = self.made.take() else { return Ok(()) };
        let Some(debug) = self.debug.clone() else { return Ok(()) };
        let (here, before) = (self.here, self.before);
        let five = debug.version >= 5;
        let address: u8 = if self.i386 { 4 } else { 8 };
        let code: Vec<(usize, u64)> =
            made.code.iter().map(|&part| (part, self.parts[part].size)).collect();
        let functions: Vec<Function> = self
            .syms
            .iter()
            .enumerate()
            .filter(|(at, sym)| {
                matches!(sym.at, Held::In { .. })
                    && matches!(sym.sort, Sort::Func | Sort::Ifunc)
                    && sym.size > 0
                    && !sym.numbered
                    && !self.equated.contains_key(at)
            })
            .map(|(_, sym)| Function {
                name: sym.name.clone(),
                global: sym.binding == Binding::Global,
                size: sym.size,
            })
            .collect();
        let globals = functions.iter().filter(|function| function.global).count();
        let external = match globals {
            0 => External::None,
            n if n == functions.len() && debug.version >= 4 => External::Present,
            _ => External::Flag,
        };

        // The names: the file, the directory, the producer and then each function.
        let mut names = Bytes::default();
        let mut name = |text: &[u8]| {
            let at = names.out.len() as u64;
            names.out.extend_from_slice(text);
            names.out.push(0);
            at
        };
        let file = name(&self.lines.first());
        let dir = name(&self.remap(&debug.dir));
        let producer = name(debug.producer.as_bytes());
        let named: Vec<u64> =
            functions.iter().map(|function| name(function.name.as_bytes())).collect();
        let strings = self.emit(made.strings, names)?;

        let abbrev = self.emit(made.abbrev, abbreviations(&functions, code.len(), external))?;

        let mut ranges_at = 0;
        if let Some(part) = made.ranges {
            let mut list = Bytes::default();
            if five {
                list.out.extend_from_slice(&[0; 4]);
                list.out.extend_from_slice(&5u16.to_le_bytes());
                list.out.extend_from_slice(&[address, 0, 0, 0, 0, 0]);
                for &(part, size) in &code {
                    // `DW_RLE_start_length`.
                    list.out.push(7);
                    list.hole(address, To::Place(part, 0));
                    uleb(&mut list.out, size);
                }
                list.out.push(0);
                list.length();
            } else {
                list.number(address, u64::MAX);
                list.number(address, 0);
                for &(part, size) in &code {
                    list.hole(address, To::Place(part, 0));
                    list.hole(address, To::Place(part, size));
                }
                list.number(address, 0);
                list.number(address, 0);
            }
            ranges_at = self.emit(part, list)? + if five { 12 } else { 0 };
            if !five {
                let part = &mut self.parts[part];
                part.align = part.align.max(2 * u64::from(address));
            }
        }

        let mut unit = Bytes::default();
        unit.out.extend_from_slice(&[0; 4]);
        unit.out.extend_from_slice(&u16::from(debug.version).to_le_bytes());
        if five {
            // `DW_UT_compile`.
            unit.out.extend_from_slice(&[1, address]);
            unit.hole(4, To::Place(made.abbrev, abbrev));
        } else {
            unit.hole(4, To::Place(made.abbrev, abbrev));
            unit.out.push(address);
        }
        uleb(&mut unit.out, 1);
        unit.hole(4, To::Place(made.line.0, made.line.1));
        match (code.as_slice(), made.ranges) {
            ([(part, size)], None) => {
                unit.hole(address, To::Place(*part, 0));
                uleb(&mut unit.out, *size);
            }
            (_, Some(ranges)) => unit.hole(4, To::Place(ranges, ranges_at)),
            _ => {}
        }
        for at in [file, dir, producer] {
            unit.hole(4, To::Place(made.strings, strings + at));
        }
        unit.out.extend_from_slice(&ASSEMBLER.to_le_bytes());
        if !functions.is_empty() {
            // Each function says where the type after them all is, and how many bytes that takes
            // moves where it is. gas works it out the same way, by growing the numbers until the
            // place they name stops moving.
            let flag = usize::from(external == External::Flag);
            let fixed: usize = functions
                .iter()
                .map(|function| 1 + 4 + flag + usize::from(address) + leb_len(function.size))
                .sum();
            let mut width = 1;
            let kind = loop {
                let kind = (unit.out.len() + fixed + width * functions.len()) as u64;
                if leb_len(kind) == width {
                    break kind;
                }
                width = leb_len(kind);
            };
            for (function, at) in functions.iter().zip(&named) {
                uleb(&mut unit.out, 2);
                unit.hole(4, To::Place(made.strings, strings + at));
                if external == External::Flag {
                    unit.out.push(u8::from(function.global));
                }
                uleb(&mut unit.out, kind);
                unit.hole(address, To::Name(function.name.clone()));
                uleb(&mut unit.out, function.size);
            }
            uleb(&mut unit.out, 3);
            unit.out.push(0);
        }
        unit.length();
        let info = self.emit(made.info, unit)?;

        let mut list = Bytes::default();
        list.out.extend_from_slice(&[0; 4]);
        list.out.extend_from_slice(&2u16.to_le_bytes());
        list.hole(4, To::Place(made.info, info));
        list.out.extend_from_slice(&[address, 0]);
        let pair = 2 * usize::from(address);
        list.out.resize(list.out.len().next_multiple_of(pair), 0);
        for &(part, size) in &code {
            list.hole(address, To::Place(part, 0));
            list.number(address, size);
        }
        list.out.extend(std::iter::repeat_n(0, pair));
        list.length();
        self.emit(made.aranges, list)?;
        let part = &mut self.parts[made.aranges];
        part.align = part.align.max(pair as u64);

        (self.here, self.before) = (here, before);
        Ok(())
    }

    /// One section's bytes, put on the end of it with a fixup for each hole, which comes back as
    /// where they start.
    fn emit(&mut self, part: usize, bytes: Bytes) -> Result<u64, Trouble> {
        self.go(part);
        let start = self.at();
        self.put(&bytes.out)?;
        for (at, width, to) in bytes.holes {
            let name = match to {
                To::Name(name) => name,
                To::Place(part, offset) => {
                    let name = format!("\u{1}debug{}", self.fixups.len());
                    let sym = self.sym(&name);
                    self.syms[sym].at = Held::In { part, offset };
                    name
                }
            };
            self.fixups.push(Fixup {
                part: self.here,
                at: start + at as u64,
                width,
                sum: Sum { constant: 0, terms: vec![Term { coeff: 1, what: What::Symbol(name) }] },
                reach: Reach::Near,
                slot: Reference::Got,
                branch: None,
                jump: false,
                field: None,
                leb: None,
                named: None,
                line: self.line,
            });
        }
        Ok(start)
    }
}

/// `.debug_abbrev` for a unit with these functions and this many sections of code, in gas's words.
fn abbreviations(functions: &[Function], sections: usize, external: External) -> Bytes {
    let mut out = vec![1, COMPILE_UNIT, u8::from(!functions.is_empty())];
    // `DW_AT_stmt_list` as `DW_FORM_sec_offset`.
    out.extend_from_slice(&[0x10, 0x17]);
    if sections == 1 {
        // `DW_AT_low_pc` as an address and `DW_AT_high_pc` as how far past it.
        out.extend_from_slice(&[0x11, 0x01, 0x12, 0x0f]);
    } else {
        // `DW_AT_ranges`.
        out.extend_from_slice(&[0x55, 0x17]);
    }
    // `DW_AT_name`, `DW_AT_comp_dir` and `DW_AT_producer` as strings, `DW_AT_language` in two bytes.
    out.extend_from_slice(&[0x03, 0x0e, 0x1b, 0x0e, 0x25, 0x0e, 0x13, 0x05, 0, 0]);
    if !functions.is_empty() {
        out.extend_from_slice(&[2, SUBPROGRAM, 0, 0x03, 0x0e]);
        match external {
            External::None => {}
            External::Present => out.extend_from_slice(&[0x3f, 0x19]),
            External::Flag => out.extend_from_slice(&[0x3f, 0x0c]),
        }
        // `DW_AT_type` as a reference, then the address and the size.
        out.extend_from_slice(&[0x49, 0x15, 0x11, 0x01, 0x12, 0x0f, 0, 0]);
        out.extend_from_slice(&[3, UNSPECIFIED_TYPE, 0, 0, 0]);
    }
    out.push(0);
    Bytes { out, holes: Vec::new() }
}

/// How many bytes a number takes as unsigned LEB128.
fn leb_len(value: u64) -> usize {
    let mut out = Vec::new();
    uleb(&mut out, value);
    out.len()
}

/// `# 12 "file.h" 1`, the marker the preprocessor leaves to say the next line is line 12 of
/// `file.h`, as the line and the file. A name in it has a backslash in front of every backslash and
/// quote.
///
/// gas takes a line as one only when it starts with the `#` and the number is followed by a name in
/// quotes. Anything else, `# 1st arg`, `# 64 bytes` or `# 20` alone, is a comment and still counts
/// as a line of the file.
fn marker(raw: &str) -> Option<(u64, String)> {
    let rest = raw.strip_prefix('#')?.trim_start_matches([' ', '\t']);
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let line = rest[..digits].parse().ok()?;
    let quoted = rest[digits..].trim_start_matches([' ', '\t']).strip_prefix('"')?;
    let mut name = String::new();
    let mut chars = quoted.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => return Some((line, name)),
            '\\' => name.push(chars.next()?),
            _ => name.push(ch),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use rucc_object::Assembled;
    use rucc_target::ObjectFormat;
    use rucc_tuple::Arch;

    use super::marker;
    use crate::source::{DebugInfo, Flags, read_with_debug};

    /// The file read under `-g` as gas 2.42 in `/tmp/k2-t/as` would read it.
    fn assembled(text: &str, arch: Arch, name: &str) -> Assembled {
        let debug = DebugInfo {
            version: 5,
            name: name.to_owned(),
            dir: "/tmp/k2-t/as".to_owned(),
            producer: "GNU AS 2.42".to_owned(),
            remap: Vec::new(),
        };
        match read_with_debug(text, arch, ObjectFormat::Elf, Flags::default(), Some(&debug)) {
            Ok(assembled) => assembled,
            Err(trouble) => panic!("line {}: {}", trouble.line, trouble.why),
        }
    }

    fn bytes<'a>(file: &'a Assembled, name: &str) -> &'a [u8] {
        let part = file.parts.iter().find(|part| part.name == name);
        part.map_or(&[], |part| part.bytes.as_slice())
    }

    /// Code in two sections and a global function, as gcc's preprocessor hands it over. The bytes
    /// are the ones gas 2.42 writes for the same file.
    #[test]
    fn a_file_of_assembly_gets_the_debug_information_gas_makes() {
        let file = assembled(
            concat!(
                "# 0 \"x.S\"\n# 0 \"<built-in>\"\n# 0 \"<command-line>\"\n",
                "# 1 \"/usr/include/stdc-predef.h\" 1 3 4\n# 0 \"<command-line>\" 2\n# 1 \"x.S\"\n",
                " .text\n .globl f\n .type f, @function\nf:\n movl $1, %eax\n ret\n",
                " .size f, .-f\n .section .init.text,\"ax\"\n nop\n",
            ),
            Arch::X86_64,
            "x.S",
        );
        #[rustfmt::skip]
        let line = [
            0x55, 0, 0, 0, 5, 0, 8, 0, 0x2a, 0, 0, 0, 1, 1, 1, 0xfb, 0xe, 0xd, 0, 1, 1, 1, 1, 0,
            0, 0, 1, 0, 0, 1, 1, 1, 0x1f, 1, 0, 0, 0, 0, 2, 1, 0x1f, 2, 0xf, 2, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 9, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0x16, 0x59, 2, 1, 0, 1, 1, 0, 9, 2, 0, 0,
            0, 0, 0, 0, 0, 0, 0x1a, 2, 1, 0, 1, 1,
        ];
        assert_eq!(bytes(&file, ".debug_line"), line);
        assert_eq!(bytes(&file, ".debug_line_str"), b"/tmp/k2-t/as\0x.S\0");
        #[rustfmt::skip]
        let info = [
            0x30, 0, 0, 0, 5, 0, 1, 8, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 1, 0x80, 2, 0, 0, 0, 0, 0x32, 0, 0, 0, 0, 0, 0, 0, 0, 6, 3, 0,
        ];
        assert_eq!(bytes(&file, ".debug_info"), info);
        #[rustfmt::skip]
        let abbrev = [
            1, 0x11, 1, 0x10, 0x17, 0x55, 0x17, 3, 0xe, 0x1b, 0xe, 0x25, 0xe, 0x13, 5, 0, 0, 2,
            0x2e, 0, 3, 0xe, 0x3f, 0x19, 0x49, 0x15, 0x11, 1, 0x12, 0xf, 0, 0, 3, 0x3b, 0, 0, 0, 0,
        ];
        assert_eq!(bytes(&file, ".debug_abbrev"), abbrev);
        assert_eq!(bytes(&file, ".debug_str"), b"x.S\0/tmp/k2-t/as\0GNU AS 2.42\0f\0");
        #[rustfmt::skip]
        let ranges = [
            0x1d, 0, 0, 0, 5, 0, 8, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 0, 6, 7, 0, 0, 0, 0, 0,
            0, 0, 0, 1, 0,
        ];
        assert_eq!(bytes(&file, ".debug_rnglists"), ranges);
        let mut aranges = vec![0x3c, 0, 0, 0, 2, 0, 0, 0, 0, 0, 8];
        aranges.resize(24, 0);
        aranges.extend_from_slice(&[6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        aranges.resize(64, 0);
        assert_eq!(bytes(&file, ".debug_aranges"), aranges);
    }

    /// Which names are functions the unit lists: a name a call makes before it is defined comes
    /// first, a weak one and a local one are not external, a name set to another with a type and a
    /// size is one, and so is one in data, but one without a size is not.
    #[test]
    fn every_function_with_a_size_is_named_in_the_order_it_was_first_written() {
        let file = assembled(
            concat!(
                " .text\n bl later\n .globl first\n .type first, %function\nfirst:\n nop\n",
                " .size first, .-first\n .weak w\n .type w, %function\nw: nop\n nop\n",
                " .size w, 8\n .type later, %function\nlater: ret\n .size later, .-later\n",
                " .type alias, %function\n .set alias, first\n .size alias, 4\n .globl alias\n",
                " .type zero, %function\nzero: nop\n .size zero, 0\n .data\n",
                " .type dfunc, %function\ndfunc: .quad 0\n .size dfunc, 8\n",
                " .type obj, %object\nobj: .quad 0\n .size obj, 8\n",
            ),
            Arch::Aarch64,
            "d.S",
        );
        assert_eq!(
            bytes(&file, ".debug_str"),
            b"d.S\0/tmp/k2-t/as\0GNU AS 2.42\0later\0first\0w\0alias\0dfunc\0"
        );
        let abbrev = bytes(&file, ".debug_abbrev");
        assert_eq!(&abbrev[24..26], [0x3f, 0x0c], "external is a flag when some are not");
        let info = bytes(&file, ".debug_info");
        assert_eq!(info.len(), 122);
        // The flag of each function, then the type, which is the last entry in the unit.
        let flags: Vec<u8> = (0..5).map(|n| info[45 + n * 16]).collect();
        assert_eq!(flags, [0, 1, 0, 1, 0]);
        assert!(info[46..].chunks(16).take(5).all(|function| function[0] == 0x78));
        // Four bytes to the instruction on AArch64, so the address advances count words.
        #[rustfmt::skip]
        let program = [0x13, 0x24, 0x24, 0x21, 0x23, 0x27, 2, 1, 0, 1, 1];
        assert!(bytes(&file, ".debug_line").ends_with(&program));
    }

    /// A file that describes its own lines keeps none of the rows made for it, and the unit is
    /// named for the first file it gave.
    #[test]
    fn a_file_with_its_own_lines_keeps_none_made() {
        let text = ".text\nnop\n.file 1 \"a.c\"\n.loc 1 7 0\nnop\n";
        let file = assembled(text, Arch::X86_64, "f.s");
        assert_eq!(bytes(&file, ".debug_str"), b"a.c\0/tmp/k2-t/as\0GNU AS 2.42\0");
        // One row, at the second instruction and line 7, whose address is in the relocation.
        #[rustfmt::skip]
        let program = [0, 9, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0x18, 2, 1, 0, 1, 1];
        assert!(bytes(&file, ".debug_line").ends_with(&program));
    }

    #[test]
    fn a_marker_is_a_line_and_a_file() {
        assert_eq!(marker("# 12 \"a\\\"b.h\" 1 3"), Some((12, "a\"b.h".to_owned())));
        assert_eq!(marker("#80 \"m.s\""), Some((80, "m.s".to_owned())));
        assert_eq!(marker("# 20"), None);
        assert_eq!(marker("  # 70 \"q.h\""), None);
        assert_eq!(marker("# define X"), None);
        assert_eq!(marker("# 1st arg is %rdi"), None);
        assert_eq!(marker("#"), None);
    }
}
