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

/// A directory of this test's own, so that two of these running at once do not write the same
/// file, with the source already in it.
fn fixture(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-dv-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    std::fs::write(dir.join("one.c"), SOURCE).expect("the fixture can be written");
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

/// Whether those bytes are somewhere in that section, and false for a section that is not there.
fn holds(object: &[u8], name: &str, want: &[u8]) -> bool {
    section(object, name).is_some_and(|bytes| bytes.windows(want.len()).any(|seen| seen == want))
}

#[test]
fn a_local_with_a_frame_slot_comes_out_named_and_placed() {
    let dir = fixture("placed");
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

/// A build with no unwind table says nothing about where a local is.
///
/// The offset is from the frame base, the frame base is the call frame address, and the call frame
/// address is what the unwind table resolves. Without one the location is an expression a debugger
/// cannot evaluate, so it is not written, and the name goes with it rather than standing there
/// with nothing under it.
#[test]
fn a_build_with_nothing_to_resolve_the_frame_base_against_places_nothing() {
    let dir = fixture("loose");
    let flags = ["-g", "-fno-asynchronous-unwind-tables", "-fno-unwind-tables"];
    let object = build(&dir, &flags, "loose.o");
    let _ = std::fs::remove_dir_all(&dir);

    assert!(!holds(&object, ".debug_str", b"running"), "a name with nowhere to be");
    assert!(!holds(&object, ".debug_abbrev", &LOCATION), "a location nothing can resolve");

    // The rest of the unit is still there, which is what says the flag took the locations out
    // rather than the debug information.
    assert!(holds(&object, ".debug_str", b"total"), "the function went with them");

    // And so are the locals that are in a register, which is the point of the rule rather than an
    // accident of it: a register is not measured from the frame base, so nothing about one needs a
    // frame base to be there.
    assert!(holds(&object, ".debug_str", b"index"), "a register location went with them");
    assert!(holds(&object, ".debug_abbrev", &LISTED), "a register location went with them");
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
    let dir = fixture("kept");
    let object = build(&dir, &["-g"], "kept.o");
    let _ = std::fs::remove_dir_all(&dir);

    assert!(holds(&object, ".debug_str", b"index"), "the local is not named");
    assert!(holds(&object, ".debug_abbrev", &LISTED), "nothing says where anything is over time");

    // And the list itself, which is what the offset above points into. A section with nothing in it
    // would be a name and an offset with no stretches under them.
    let list = section(&object, ".debug_loclists");
    assert!(list.is_some_and(|bytes| !bytes.is_empty()), "the stretches are not written down");
}
