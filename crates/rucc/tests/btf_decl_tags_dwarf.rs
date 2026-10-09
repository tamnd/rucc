//! `btf_decl_tag` in the debugging information, read back out of the object.
//!
//! The fixtures are gcc 16's own tests of what it writes for the tag,
//! `gcc.dg/debug/dwarf2/dwarf-btf-decl-tag-*.c`, and what is held against each is what those tests
//! count: how many `DW_TAG_GNU_annotation` entries there are, and how many `DW_AT_GNU_annotation`
//! attributes point at one. pahole reads the same entries into the kernel's BTF, so a count that
//! differs is a BTF that differs.
//!
//! The reader is a small one over a 64 bit little endian ELF object and the forms this compiler
//! writes, and nothing else.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

const TARGET: &str = "--target=x86_64-unknown-linux-gnu";

/// `DW_TAG_GNU_annotation`.
const ANNOTATION: u64 = 0x6001;

/// `DW_AT_GNU_annotation`.
const ANNOTATED: u64 = 0x2139;

/// What the tests below read off one entry.
#[derive(Debug, Default)]
struct Entry {
    at: u64,
    tag: u64,
    name: Option<String>,
    value: Option<String>,
    annotation: Option<u64>,
}

/// The object `source` compiles to with `-g`.
fn build(what: &str, source: &str) -> Vec<u8> {
    let name = format!("rucc-btf-dwarf-{}-{what}", std::process::id());
    let dir: PathBuf = std::env::temp_dir().join(name);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    std::fs::write(dir.join("one.c"), source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .current_dir(&dir)
        .args([TARGET, "-g", "-c", "-o", "one.o", "one.c"])
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

/// One section of the object, and nothing for one that is not there.
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

/// A string out of `.debug_str`.
fn string(strings: &[u8], offset: u64) -> String {
    let from = &strings[offset as usize..];
    let len = from.iter().position(|&byte| byte == 0).expect("a string ends");
    String::from_utf8_lossy(&from[..len]).into_owned()
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
    let mut out = Vec::new();
    while at < end {
        let offset = at as u64;
        let code = leb(info, &mut at);
        if code == 0 {
            continue;
        }
        let (tag, _, attrs) = &abbrevs[&code];
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
                (0x03, 0x0e, Some(off)) => entry.name = Some(string(strings, off)),
                (0x1c, 0x0e, Some(off)) => entry.value = Some(string(strings, off)),
                (ANNOTATED, _, Some(to)) => entry.annotation = Some(to),
                _ => {}
            }
        }
        out.push(entry);
    }
    out
}

/// How many annotations there are and how many attributes point at one, which are the two
/// numbers gcc's tests count.
fn counts(all: &[Entry]) -> (usize, usize) {
    let tags = all.iter().filter(|entry| entry.tag == ANNOTATION).count();
    let pointing = all.iter().filter(|entry| entry.annotation.is_some()).count();
    (tags, pointing)
}

/// The strings of the chain the entry of that name points at, in the order it goes.
fn chain(all: &[Entry], name: &str) -> Vec<String> {
    let entry = all
        .iter()
        .find(|entry| entry.tag != ANNOTATION && entry.name.as_deref() == Some(name))
        .unwrap_or_else(|| panic!("no entry for {name}"));
    let mut out = Vec::new();
    let mut next = entry.annotation;
    while let Some(at) = next {
        let tag = all.iter().find(|entry| entry.at == at).expect("a reference names an entry");
        assert_eq!(tag.tag, ANNOTATION, "{name} points at something other than a tag");
        assert_eq!(tag.name.as_deref(), Some("btf_decl_tag"));
        out.push(tag.value.clone().expect("a tag holds its string"));
        next = tag.annotation;
    }
    out
}

/// `dwarf-btf-decl-tag-1.c`: one tag is one annotation the variable points at.
#[test]
fn one_tag_is_one_annotation() {
    let all = entries(&build("one", "int *foo __attribute__((btf_decl_tag (\"my_foo\")));\n"));
    assert_eq!(counts(&all), (1, 1));
    assert_eq!(chain(&all, "foo"), ["my_foo"]);
}

/// `dwarf-btf-decl-tag-2.c`: the members carry their tags, and the variable's two share the
/// end of their chain with the members that carry the one.
#[test]
fn a_member_carries_its_tags_and_shares_them() {
    let source = r#"
#define __tag1 __attribute__((btf_decl_tag ("decl1")))
#define __tag2 __attribute__((btf_decl_tag ("decl2")))

union U {
  int i __tag1;
  unsigned char ub[4];
};

struct S {
  union U u;
  int b __tag2;
  char *z __tag1;
};

struct S my_s __tag1 __tag2;
"#;
    let all = entries(&build("members", source));
    assert_eq!(counts(&all), (3, 5));
    assert_eq!(chain(&all, "my_s"), ["decl2", "decl1"]);
    assert_eq!(chain(&all, "i"), ["decl1"]);
    assert_eq!(chain(&all, "b"), ["decl2"]);
    assert_eq!(chain(&all, "z"), ["decl1"]);
}

/// `dwarf-btf-decl-tag-3.c`: a function and its parameters carry theirs.
#[test]
fn a_function_and_its_parameters_carry_their_tags() {
    let source = r#"
#define __tag1 __attribute__((btf_decl_tag ("decl1")))
#define __tag2 __attribute__((btf_decl_tag ("decl2")))

int __tag1 __tag2 func (int arg_a __tag1, int arg_b __tag2)
{
  return arg_a * arg_b;
}

int foo (int x) {
  return func (x, x + 1);
}
"#;
    let all = entries(&build("function", source));
    assert_eq!(counts(&all), (3, 4));
    assert_eq!(chain(&all, "func"), ["decl2", "decl1"]);
    assert_eq!(chain(&all, "arg_a"), ["decl1"]);
    assert_eq!(chain(&all, "arg_b"), ["decl2"]);
    assert_eq!(chain(&all, "foo"), Vec::<String>::new());
}

/// `dwarf-btf-decl-tag-4.c`: a variable declared again keeps the tags of every declaration.
#[test]
fn a_variable_declared_again_keeps_every_tag() {
    let source = r#"
#define __tag1 __attribute__((btf_decl_tag ("tag1")))
#define __tag2 __attribute__((btf_decl_tag ("tag2")))
#define __tag3 __attribute__((btf_decl_tag ("tag3")))
#define __tag4 __attribute__((btf_decl_tag ("tag4")))

int foo __tag1;
int foo __tag2;

int bar __tag3;
int bar;

int baz;
int baz __tag4;
"#;
    let all = entries(&build("again", source));
    assert_eq!(counts(&all), (4, 4));
    assert_eq!(chain(&all, "foo"), ["tag2", "tag1"]);
    assert_eq!(chain(&all, "bar"), ["tag3"]);
    assert_eq!(chain(&all, "baz"), ["tag4"]);
}

/// `dwarf-btf-decl-tag-6.c`: a function defined after a declaration has the tags of both, the
/// declaration's in front of the definition's, which is where gcc's merge puts the shorter list.
#[test]
fn a_function_defined_after_its_declaration_keeps_every_tag() {
    let source = r#"
#define __tag1 __attribute__((btf_decl_tag ("tag1")))
#define __tag2 __attribute__((btf_decl_tag ("tag2")))
#define __tag3 __attribute__((btf_decl_tag ("tag3")))

__tag1
extern int
do_thing (int);

__tag2
__tag3
int
do_thing (int x)
{
  return x * x;
}
"#;
    let all = entries(&build("defined", source));
    assert_eq!(counts(&all), (3, 3));
    assert_eq!(chain(&all, "do_thing"), ["tag1", "tag3", "tag2"]);
}

/// A local carries its tags too, and a tag gcc refuses or ignores is not written. The local's
/// address is taken so that it is in the frame, where it gets an entry.
#[test]
fn a_local_carries_its_tags_and_a_refused_one_is_not_written() {
    let source = r#"
struct __attribute__((btf_decl_tag("type"))) T { int x; };
int g(int *);
int f(void) {
  int l __attribute__((btf_decl_tag("local"), btf_decl_tag("local"))) = 1;
  struct T t = { g(&l) };
  return t.x;
}
"#;
    let all = entries(&build("local", source));
    assert_eq!(counts(&all), (1, 1));
    assert_eq!(chain(&all, "l"), ["local"]);
}
