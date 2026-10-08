//! The type entries `-g` writes, read back the way pahole reads them.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.4.
//!
//! The kernel's BTF is pahole's reading of the DWARF in `vmlinux`, and a BPF program is built against
//! that BTF by name. So a kernel built with this compiler has the same BTF as one built with gcc only
//! where both write the same type entries, and each test below is one way the two used to differ:
//! a typedef written over another typedef, a type nothing in the unit uses, one function type
//! written two ways, a bit-field with no name, an enumeration never completed and an array of
//! arrays. What gcc writes for each is what is asserted.
//!
//! The entries are read out of `.debug_info` by hand, with only the forms this compiler writes,
//! because this crate depends on the driver and on nothing else.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

const TARGET: &str = "--target=aarch64-unknown-linux-gnu";

const DW_TAG_ARRAY: u64 = 0x01;
const DW_TAG_ENUMERATION: u64 = 0x04;
const DW_TAG_FORMAL_PARAMETER: u64 = 0x05;
const DW_TAG_MEMBER: u64 = 0x0d;
const DW_TAG_STRUCTURE: u64 = 0x13;
const DW_TAG_SUBROUTINE: u64 = 0x15;
const DW_TAG_TYPEDEF: u64 = 0x16;
const DW_TAG_VARIABLE: u64 = 0x34;
const DW_TAG_SUBRANGE: u64 = 0x21;
const DW_TAG_BASE: u64 = 0x24;
const DW_TAG_CONST: u64 = 0x26;

/// One entry: where it is in the unit, its tag, its name, what its `DW_AT_type` names, and the
/// entries under it.
#[derive(Debug, Default)]
struct Entry {
    at: u64,
    tag: u64,
    name: Option<String>,
    ty: Option<u64>,
    declaration: bool,
    children: Vec<usize>,
}

/// The object `source` compiles to with `-g -O2`.
fn build(what: &str, source: &str) -> Vec<u8> {
    let dir: PathBuf = std::env::temp_dir().join(format!("rucc-dg-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    std::fs::write(dir.join("one.c"), source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([TARGET, "-std=gnu11", "-g", "-O2", "-c", "-o"])
        .arg(dir.join("one.o"))
        .arg(dir.join("one.c"))
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let object = std::fs::read(dir.join("one.o")).expect("the object was written");
    let _ = std::fs::remove_dir_all(&dir);
    object
}

fn number(bytes: &[u8], at: usize, len: usize) -> u64 {
    let mut out = [0u8; 8];
    out[..len].copy_from_slice(&bytes[at..at + len]);
    u64::from_le_bytes(out)
}

/// One section of a 64 bit little endian ELF file, and nothing for one that is not there.
fn section<'a>(object: &'a [u8], want: &str) -> Option<&'a [u8]> {
    let start = number(object, 0x28, 8) as usize;
    let size = number(object, 0x3a, 2) as usize;
    let count = number(object, 0x3c, 2) as usize;
    let names = number(object, 0x3e, 2) as usize;
    let at = |which: usize| start + which * size;
    let strings = number(object, at(names) + 24, 8) as usize;
    (0..count).find_map(|which| {
        let header = at(which);
        let name = strings + number(object, header, 4) as usize;
        let end = object[name..].iter().position(|&byte| byte == 0).expect("a name ends");
        let offset = number(object, header + 24, 8) as usize;
        let len = number(object, header + 32, 8) as usize;
        (&object[name..name + end] == want.as_bytes()).then(|| &object[offset..offset + len])
    })
}

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

/// What one abbreviation says: the tag, whether it has children, and its attributes as pairs of
/// name and form.
type Abbreviation = (u64, bool, Vec<(u64, u64)>);

/// What each abbreviation code says.
fn abbreviations(bytes: &[u8]) -> HashMap<u64, Abbreviation> {
    let mut out = HashMap::new();
    let mut at = 0;
    loop {
        let code = leb(bytes, &mut at);
        if code == 0 {
            return out;
        }
        let tag = leb(bytes, &mut at);
        let children = bytes[at] != 0;
        at += 1;
        let mut attrs = Vec::new();
        loop {
            let (attr, form) = (leb(bytes, &mut at), leb(bytes, &mut at));
            if attr == 0 && form == 0 {
                break;
            }
            if form == 0x21 {
                leb(bytes, &mut at);
            }
            attrs.push((attr, form));
        }
        out.insert(code, (tag, children, attrs));
    }
}

/// Every entry in the object's one unit, in the order they were written.
fn entries(object: &[u8]) -> Vec<Entry> {
    let info = section(object, ".debug_info").expect("a unit");
    let abbrevs = abbreviations(section(object, ".debug_abbrev").expect("abbreviations"));
    let strings = section(object, ".debug_str").unwrap_or_default();
    // An offset into `.debug_str` is a relocation in an object, so its addend is the offset.
    let relocated: HashMap<usize, u64> = section(object, ".rela.debug_info")
        .unwrap_or_default()
        .chunks(24)
        .map(|rela| (number(rela, 0, 8) as usize, number(rela, 16, 8)))
        .collect();
    let address = usize::from(info[7]);
    let end = 4 + number(info, 0, 4) as usize;
    let mut at = 12;
    let mut out: Vec<Entry> = Vec::new();
    let mut parents: Vec<Option<usize>> = Vec::new();
    while at < end {
        let offset = at as u64;
        let code = leb(info, &mut at);
        if code == 0 {
            parents.pop();
            continue;
        }
        let (tag, children, attrs) = &abbrevs[&code];
        let mut entry = Entry { at: offset, tag: *tag, ..Entry::default() };
        for &(attr, form) in attrs {
            let value = match form {
                0x01 => Some(number(info, at, address)).inspect(|_| at += address),
                0x0b | 0x0c | 0x11 => Some(number(info, at, 1)).inspect(|_| at += 1),
                0x05 | 0x12 => Some(number(info, at, 2)).inspect(|_| at += 2),
                0x06 | 0x0e | 0x13 | 0x17 | 0x1f => {
                    let value = relocated.get(&at).copied().unwrap_or(number(info, at, 4));
                    at += 4;
                    Some(value)
                }
                0x07 | 0x14 => Some(number(info, at, 8)).inspect(|_| at += 8),
                0x0f | 0x0d | 0x15 => Some(leb(info, &mut at)),
                0x18 | 0x09 => {
                    let len = leb(info, &mut at) as usize;
                    at += len;
                    None
                }
                0x0a => {
                    at += 1 + usize::from(info[at]);
                    None
                }
                0x19 | 0x21 => None,
                other => panic!("form {other:#x} is not one this reader knows"),
            };
            match (attr, form, value) {
                (0x03, 0x0e, Some(off)) => {
                    let from = &strings[off as usize..];
                    let len = from.iter().position(|&byte| byte == 0).expect("a name ends");
                    entry.name = Some(String::from_utf8_lossy(&from[..len]).into_owned());
                }
                (0x49, _, Some(to)) => entry.ty = Some(to),
                (0x3c, _, _) => entry.declaration = true,
                _ => {}
            }
        }
        let index = out.len();
        if let Some(&Some(parent)) = parents.last() {
            out[parent].children.push(index);
        }
        out.push(entry);
        if *children {
            parents.push(Some(index));
        }
    }
    out
}

/// The entry at a unit offset, which is what a `DW_AT_type` holds.
fn at(all: &[Entry], offset: u64) -> &Entry {
    all.iter().find(|entry| entry.at == offset).expect("a reference names an entry")
}

fn named<'a>(all: &'a [Entry], tag: u64, name: &str) -> Option<&'a Entry> {
    all.iter().find(|entry| entry.tag == tag && entry.name.as_deref() == Some(name))
}

/// `typedef __u64 u64;` is a typedef over a typedef, and pahole says so in the BTF.
#[test]
fn a_typedef_written_over_another_names_the_other() {
    let source = "typedef unsigned long long __u64;\ntypedef __u64 u64;\nu64 v;\nint f(void) { return 0; }\n";
    let all = entries(&build("chain", source));
    let outer = named(&all, DW_TAG_TYPEDEF, "u64").expect("u64 is described");
    let inner = at(&all, outer.ty.expect("u64 has a type"));
    assert_eq!((inner.tag, inner.name.as_deref()), (DW_TAG_TYPEDEF, Some("__u64")));
    let base = at(&all, inner.ty.expect("__u64 has a type"));
    assert_eq!((base.tag, base.name.as_deref()), (DW_TAG_BASE, Some("long long unsigned int")));
}

/// A type that only a `static inline` nobody called mentions has no entry, which is gcc's answer.
#[test]
fn a_type_nothing_emitted_uses_is_left_out() {
    let source = "\
struct unused { int x; };
enum never { NEVER_A };
static inline int helper(struct unused *u) { enum never n = NEVER_A; return u->x + n; }
int used(int a) { return a + 1; }
";
    let all = entries(&build("unused", source));
    assert!(named(&all, DW_TAG_STRUCTURE, "unused").is_none());
    assert!(named(&all, DW_TAG_ENUMERATION, "never").is_none());
    assert!(named(&all, DW_TAG_BASE, "int").is_some(), "what `used` takes is still there");
}

/// One function type written two ways in two members is two entries, each the way it was written.
#[test]
fn a_prototype_is_described_the_way_each_declaration_wrote_it() {
    let source = "\
typedef unsigned int u32;
typedef unsigned long size_t;
struct a { void (*f)(u32); };
struct b { void (*g)(unsigned int); };
struct c { long (*read)(const size_t); };
struct a x; struct b y; struct c z;
int f(void) { return 0; }
";
    let all = entries(&build("prototypes", source));
    let takes = |which: &Entry| -> Vec<(u64, Option<String>)> {
        which
            .children
            .iter()
            .map(|&child| &all[child])
            .filter(|child| child.tag == DW_TAG_FORMAL_PARAMETER)
            .map(|param| {
                let ty = at(&all, param.ty.expect("a parameter has a type"));
                (ty.tag, ty.name.clone())
            })
            .collect()
    };
    let shapes: Vec<_> =
        all.iter().filter(|entry| entry.tag == DW_TAG_SUBROUTINE).map(takes).collect();
    assert!(shapes.contains(&vec![(DW_TAG_TYPEDEF, Some("u32".to_owned()))]), "{shapes:?}");
    let plain = vec![(DW_TAG_BASE, Some("unsigned int".to_owned()))];
    assert!(shapes.contains(&plain), "{shapes:?}");
    assert!(shapes.contains(&vec![(DW_TAG_CONST, None)]), "{shapes:?}");
}

/// A bit-field with no name is padding and gets no member, as with gcc.
#[test]
fn an_unnamed_bit_field_has_no_member() {
    let source = "struct s { int a : 3; int : 0; int b : 3; } v;\nint f(void) { return 0; }\n";
    let all = entries(&build("bits", source));
    let record = named(&all, DW_TAG_STRUCTURE, "s").expect("s is described");
    let members: Vec<_> = record
        .children
        .iter()
        .map(|&child| &all[child])
        .filter(|child| child.tag == DW_TAG_MEMBER)
        .map(|member| member.name.clone())
        .collect();
    assert_eq!(members, [Some("a".to_owned()), Some("b".to_owned())]);
}

/// An enumeration nothing completed is a declaration, and the member that mentions it stays.
#[test]
fn an_enumeration_never_completed_is_a_declaration() {
    let source = "enum later;\nstruct s { int (*get)(enum later, int *); int x; } v;\nint f(void) { return 0; }\n";
    let all = entries(&build("later", source));
    let later = named(&all, DW_TAG_ENUMERATION, "later").expect("later is described");
    assert!(later.declaration);
    assert!(named(&all, DW_TAG_MEMBER, "get").is_some());
}

/// `int m[3][4]` is one array entry with a range for each dimension, which pahole reads as twelve.
#[test]
fn an_array_of_arrays_is_one_entry() {
    let all = entries(&build("matrix", "int m[3][4];\nint f(void) { return m[1][2]; }\n"));
    let arrays: Vec<_> = all.iter().filter(|entry| entry.tag == DW_TAG_ARRAY).collect();
    assert_eq!(arrays.len(), 1);
    let ranges = arrays[0].children.iter().filter(|&&child| all[child].tag == DW_TAG_SUBRANGE);
    assert_eq!(ranges.count(), 2);
    let element = at(&all, arrays[0].ty.expect("an element type"));
    assert_eq!(element.name.as_deref(), Some("int"));
}

/// A file with variables and no code still describes them, as gcc does: the kernel has tables of
/// structures in files like that, and their types reach the BTF only through these entries.
#[test]
fn a_file_of_only_variables_still_describes_them() {
    let source =
        "struct op { int code; const char *name; };\nconst struct op ops[2] = { { 1, \"a\" } };\n";
    let all = entries(&build("data", source));
    let ops = named(&all, DW_TAG_VARIABLE, "ops").expect("ops is described");
    let array = at(&all, ops.ty.expect("ops has a type"));
    assert_eq!(array.tag, DW_TAG_ARRAY);
    assert!(named(&all, DW_TAG_STRUCTURE, "op").is_some());
}
