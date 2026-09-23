//! What `-g` says about a local the program declared, which is its name, its type, the line it was
//! written on and where in the frame to find it.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.4. See tamnd/rucc#1645.
//!
//! This is the end of a long wire. The front end numbers a declaration, the lowering writes that
//! number on the memory it asks for, the frame layout works out how far below the call frame
//! address the memory ended up, and the driver joins the number back to the name and the type the
//! front end had. Every one of those steps has a test of its own where it lives, and none of them
//! says the wire is connected. That is what this file is for, and it is the only test that fails if
//! any link in it is dropped.
//!
//! Both halves of that are here, because there are two of them. A local whose address is taken or
//! which is too big for a register gets a frame slot, and one expression says where it is for the
//! whole function. A scalar the program never took the address of is held in an SSA value instead,
//! and where one of those is changes from one program counter to the next, so it gets a list of
//! stretches worked out from the register allocator's answer. The fixture below has both, which is
//! why it takes an address: a function of nothing but scalars would say nothing about the first
//! kind and one of nothing but arrays would say nothing about the second.
//!
//! And which scope it was declared in, which is the third thing, because a name declared inside a
//! `{ ... }` is not the same name as one of the same spelling declared outside it and a debugger
//! that cannot tell them apart prints the wrong one.
//!
//! The assertions read the sections out of the object rather than searching the whole file for the
//! bytes, which the other tests here do. Two of the three things checked are a byte or two long,
//! and a byte pair occurs everywhere in a page of machine code, so a search over the file would
//! pass whether or not anything wrote them.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A program with a local that gets a slot and a parameter that gets one.
///
/// `running` is an array, so it is in memory whatever the lowering does with it. `counter` is a
/// parameter whose address is taken, which is the case that has to go on the entry the signature
/// already wrote rather than getting a second entry of the same name.
const SOURCE: &str = "\
int twice(int counter) {
    int *pointing = &counter;
    return *pointing + counter;
}
int total(const int *of, int many) {
    int running[4] = {0, 0, 0, 0};
    for (int index = 0; index < many; index++) running[index & 3] += twice(of[index]);
    return running[0] + running[1] + running[2] + running[3];
}
";

/// The target the object is built for, written down rather than taken from the host, because an
/// object is only produced for the one this compiler has a back end for.
const TARGET: &str = "--target=x86_64-unknown-linux-gnu";

/// The attribute that says where something is, and the form it is written in, as `.debug_abbrev`
/// holds them: a pair of unsigned LEB128s, both of which are one byte at these values.
const LOCATION: [u8; 2] = [0x02, 0x18];

/// A location expression of one `DW_OP_fbreg`, as far as it can be checked without knowing the
/// offset: a length of two and the operation. What follows is a signed LEB128 the frame layout
/// decided, and writing down what it should be would be a test of the frame layout rather than of
/// this.
const FBREG: [u8; 2] = [0x02, 0x91];

/// The same attribute written as a list rather than as an expression, which is `DW_FORM_sec_offset`
/// and is an offset into `.debug_loclists`. A local the allocator kept in a register is somewhere
/// over part of a function rather than over the whole of it, so this is the form it gets.
const LISTED: [u8; 2] = [0x02, 0x17];

/// A program that declares something inside a `{ ... }` of its own, and something inside that.
///
/// `held` is an array so that it is in the frame whatever the lowering does with it, which is what
/// makes the outer block a block worth writing whichever way the allocator goes. `total` is
/// written straight into the body and is the one in here that should not end up under a block.
const NESTED: &str = "\
int sum(int many) {
    int total = 0;
    {
        int held[2] = {many, 1};
        {
            int inner = held[0] + held[1];
            total += inner;
        }
        total += held[0];
    }
    return total;
}
";

/// A program with blocks in it that declare nothing.
///
/// Every `if` and every loop body in C is a block, and a build that wrote an entry for each of
/// them would put one around most of the instructions in most programs and say nothing by it. A
/// block is only worth an entry when a name is declared in it, because telling two names apart is
/// the whole of what the entry is for.
const PLAIN: &str = "\
int plain(int many) {
    int total = 0;
    if (many > 0) { total += many; }
    while (total > 100) { total -= 3; }
    return total;
}
";

/// A directory of this test's own, so that two of these running at once do not write the same
/// file, with the source already in it.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-dv-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    std::fs::write(dir.join("one.c"), source).expect("the fixture can be written");
    dir
}

/// That object, built with those flags.
fn build(dir: &Path, flags: &[&str], object: &str) -> Vec<u8> {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([TARGET, "-c"])
        .args(flags)
        .arg("-o")
        .arg(dir.join(object))
        .arg(dir.join("one.c"))
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    std::fs::read(dir.join(object)).expect("the object was written")
}

/// Four bytes at that offset, as a number.
fn four(bytes: &[u8], at: usize) -> usize {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes")) as usize
}

/// Eight bytes at that offset, as a number.
fn eight(bytes: &[u8], at: usize) -> usize {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("eight bytes")) as usize
}

/// The contents of one section of an ELF file, and nothing for a section that is not there.
///
/// The header is at `e_shoff`, each entry is `e_shentsize` long and there are `e_shnum` of them,
/// and entry `e_shstrndx` holds the names. That is the whole of what is needed here, so it is
/// written out rather than pulled in: this crate depends on the driver and on nothing else, and a
/// test is a poor reason to change that.
fn section<'a>(object: &'a [u8], want: &str) -> Option<&'a [u8]> {
    let start = eight(object, 0x28);
    let size = u16::from_le_bytes(object[0x3a..0x3c].try_into().expect("two bytes")) as usize;
    let count = u16::from_le_bytes(object[0x3c..0x3e].try_into().expect("two bytes")) as usize;
    let names = u16::from_le_bytes(object[0x3e..0x40].try_into().expect("two bytes")) as usize;
    let at = |which: usize| start + which * size;
    let strings = eight(object, at(names) + 24);
    (0..count).find_map(|which| {
        let header = at(which);
        let name = strings + four(object, header);
        let end = object[name..].iter().position(|&byte| byte == 0).expect("a name ends");
        (&object[name..name + end] == want.as_bytes())
            .then(|| &object[eight(object, header + 24)..][..eight(object, header + 32)])
    })
}

/// One unsigned LEB128, read from `at` and leaving it just after the number.
fn leb(bytes: &[u8], at: &mut usize) -> u64 {
    let (mut out, mut shift) = (0u64, 0);
    while let Some(&byte) = bytes.get(*at) {
        *at += 1;
        out |= u64::from(byte & 0x7f) << shift;
        shift += 7;
        if byte & 0x80 == 0 {
            break;
        }
    }
    out
}

/// Every tag the unit's abbreviation table has an entry for.
///
/// An entry is a code, a tag, a byte saying whether entries of that shape have children, and then
/// pairs of an attribute and a form until a pair of zeros. All of the numbers are unsigned LEB128s
/// and a zero where a code would be is the end of the table. One file is compiled here so there is
/// one table, and nothing below needs the attributes, so they are walked past rather than kept.
fn tags(object: &[u8]) -> Vec<u64> {
    let Some(bytes) = section(object, ".debug_abbrev") else { return Vec::new() };
    let mut out = Vec::new();
    let mut at = 0;
    loop {
        if leb(bytes, &mut at) == 0 {
            return out;
        }
        out.push(leb(bytes, &mut at));
        at += 1;
        loop {
            let (attr, form) = (leb(bytes, &mut at), leb(bytes, &mut at));
            if attr == 0 && form == 0 {
                break;
            }
            // `DW_FORM_implicit_const` carries its value here rather than on the entry, so there
            // is a third number in the pair and walking past two of them would lose the place.
            if form == 0x21 {
                leb(bytes, &mut at);
            }
        }
    }
}

/// Whether those bytes are somewhere in that section, and false for a section that is not there.
fn holds(object: &[u8], name: &str, want: &[u8]) -> bool {
    section(object, name).is_some_and(|bytes| bytes.windows(want.len()).any(|seen| seen == want))
}

#[test]
fn a_local_with_a_frame_slot_comes_out_named_and_placed() {
    let dir = fixture("placed", SOURCE);
    let object = build(&dir, &["-g"], "with.o");
    let _ = std::fs::remove_dir_all(&dir);

    // The name is the half of it that says the driver found the declaration the number was for.
    // Nothing else in a build of this program writes that word, because the local is the only
    // thing called it and a build with no debug information has no names in it beyond the two
    // functions.
    assert!(holds(&object, ".debug_str", b"running"), "the local is not named");

    // And the location is the other half, which is the number the frame layout worked out. Both
    // are checked, because a name with no location is a debugger saying it cannot find a variable
    // it can see, which is worse than one that never heard of it.
    assert!(holds(&object, ".debug_abbrev", &LOCATION), "nothing says where anything is");
    assert!(holds(&object, ".debug_info", &FBREG), "the location is not an offset in the frame");
}

/// A build with no unwind table still says where a local is, through `.debug_frame`.
///
/// The offset is from the frame base, the frame base is the call frame address, and the call frame
/// address is what a table of frame rules resolves. Without an unwind table the same rules go in
/// `.debug_frame`, so the location is one a debugger can evaluate and is written as it is in any
/// other build.
#[test]
fn a_build_with_no_unwind_table_places_a_local_through_the_debug_frame() {
    let dir = fixture("loose", SOURCE);
    let flags = ["-g", "-fno-asynchronous-unwind-tables", "-fno-unwind-tables"];
    let object = build(&dir, &flags, "loose.o");
    let _ = std::fs::remove_dir_all(&dir);

    assert!(section(&object, ".debug_frame").is_some(), "no table to resolve the frame base");
    assert!(holds(&object, ".debug_str", b"running"), "the local is not named");
    assert!(holds(&object, ".debug_info", &FBREG), "the location is not an offset in the frame");

    // And the locals that are in a register, which never needed a frame base to begin with.
    assert!(holds(&object, ".debug_str", b"index"), "a register location went missing");
    assert!(holds(&object, ".debug_abbrev", &LISTED), "a register location went missing");
}

/// A local the program kept in a register comes out named and placed stretch by stretch.
///
/// The other end of the same wire, and a longer one. The front end says which declaration each of
/// its values is a value of, selection carries that to the register the value is computed into, the
/// allocator says where that register went and for how long, the assembler gives every instruction
/// an address, and the driver turns a pair of instructions into a pair of addresses. `index` is the
/// one asserted on because it is a scalar the program never takes the address of, so it is in a
/// value at every optimization level including this one.
#[test]
fn a_local_the_program_kept_in_a_register_comes_out_named_and_placed_over_stretches() {
    let dir = fixture("kept", SOURCE);
    let object = build(&dir, &["-g"], "kept.o");
    let _ = std::fs::remove_dir_all(&dir);

    assert!(holds(&object, ".debug_str", b"index"), "the local is not named");
    assert!(holds(&object, ".debug_abbrev", &LISTED), "nothing says where anything is over time");

    // And the list itself, which is what the offset above points into. A section with nothing in it
    // would be a name and an offset with no stretches under them.
    let list = section(&object, ".debug_loclists");
    assert!(list.is_some_and(|bytes| !bytes.is_empty()), "the stretches are not written down");
}

/// `DW_TAG_lexical_block`, which is the entry a name declared inside a `{ ... }` hangs off.
const BLOCK: u64 = 0x0b;

/// A name declared in an inner scope is written inside a block rather than beside the function.
///
/// What that buys is the question of which `i` a debugger means. Two blocks of one function that
/// each declare one are two variables of the same name, and with both of them children of the
/// subprogram a reader has no way to pick between them at the address it stopped at. Under a block
/// with addresses on it there is one answer.
///
/// What is asserted here is that the wire runs: the front end's scopes reached the writer and a
/// real object came out with a block in it holding the name that was declared inside one. The
/// shape of the tree is checked in `rucc-debug`, against the relocations the entries ask for,
/// which is a thing that can be read without a DWARF reader in the test.
#[test]
fn a_name_declared_in_an_inner_scope_is_written_inside_a_block() {
    let dir = fixture("nested", NESTED);
    let object = build(&dir, &["-g"], "nested.o");
    let _ = std::fs::remove_dir_all(&dir);

    assert!(tags(&object).contains(&BLOCK), "nothing wrote a scope at all");
    assert!(holds(&object, ".debug_str", b"inner"), "the name in the inner scope is not there");
    assert!(holds(&object, ".debug_str", b"held"), "the name in the outer scope is not there");

    // And the addresses under it, which are what make it an answer rather than a label. A block
    // with nothing saying where it is covers whatever its parent covers, which is the function,
    // which is the thing this is meant to stop.
    let ranges = section(&object, ".debug_rnglists");
    assert!(ranges.is_some_and(|bytes| !bytes.is_empty()), "the scope covers nothing");
}

/// A block that declares nothing gets no entry.
///
/// Every `if` and every loop body is one of these, so a build that wrote them all would pay an
/// entry for each and buy nothing: the reason to write a block down is that a name in it would
/// otherwise be confused with a name outside it, and a block holding no name has none to confuse.
#[test]
fn a_block_with_nothing_declared_in_it_is_not_written_down() {
    let dir = fixture("plain", PLAIN);
    let object = build(&dir, &["-g"], "plain.o");
    let _ = std::fs::remove_dir_all(&dir);

    assert!(!tags(&object).contains(&BLOCK), "a scope nothing was declared in was written");

    // The function is still described, which is what says the blocks were left out rather than
    // the debug information.
    assert!(holds(&object, ".debug_str", b"plain"), "the function went with them");
    assert!(holds(&object, ".debug_str", b"total"), "a local of the body went with them");
}
