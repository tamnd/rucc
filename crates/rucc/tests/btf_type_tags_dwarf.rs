//! `btf_type_tag` in the debugging information, read back out of the object.
//!
//! The fixtures are gcc 16's own tests of what it writes for the tag,
//! `gcc.dg/debug/dwarf2/dwarf-btf-type-tag-*.c`, and what is held against each is what those tests
//! count, which is how many `DW_TAG_GNU_annotation` entries there are and how many
//! `DW_AT_GNU_annotation` attributes point at one. Beside the counts is the type of each
//! declaration the way [`spell`] writes it, with the tags of each level in braces, and each is the
//! one gcc 16 writes for the same source, read off its object with `objdump --dwarf=info`.
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
    ty: Option<u64>,
}

/// The object `source` compiles to with `-g`, and what the compiler said.
fn build(what: &str, source: &str) -> (Vec<u8>, String) {
    let name = format!("rucc-btf-type-dwarf-{}-{what}", std::process::id());
    let dir: PathBuf = std::env::temp_dir().join(name);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    std::fs::write(dir.join("one.c"), source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .current_dir(&dir)
        .args([TARGET, "-std=gnu23", "-g", "-c", "-o", "one.o", "one.c"])
        .output()
        .expect("the compiler is built before its own tests run");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{said}");
    let object = std::fs::read(dir.join("one.o")).expect("the object was written");
    let _ = std::fs::remove_dir_all(&dir);
    (object, said)
}

/// The entries of what `source` compiles to, which it has to compile to without a word.
fn quiet(what: &str, source: &str) -> Vec<Entry> {
    let (object, said) = build(what, source);
    assert!(said.is_empty(), "{what}:\n{said}");
    entries(&object)
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
                (0x49, _, Some(to)) => entry.ty = Some(to),
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

fn at(all: &[Entry], offset: u64) -> &Entry {
    all.iter().find(|entry| entry.at == offset).expect("a reference names an entry")
}

/// The entry of that name that is not a tag, the first one written.
fn named<'a>(all: &'a [Entry], name: &str) -> &'a Entry {
    all.iter()
        .find(|entry| entry.tag != ANNOTATION && entry.name.as_deref() == Some(name))
        .unwrap_or_else(|| panic!("no entry for {name}"))
}

/// The strings of the chain from that annotation on, in the order it goes, every one of them a
/// tag of that name.
fn chain(all: &[Entry], mut next: Option<u64>, name: &str) -> Vec<String> {
    let mut out = Vec::new();
    while let Some(offset) = next {
        let tag = at(all, offset);
        assert_eq!(tag.tag, ANNOTATION, "a chain goes through something other than a tag");
        assert_eq!(tag.name.as_deref(), Some(name));
        out.push(tag.value.clone().expect("a tag holds its string"));
        next = tag.annotation;
    }
    out
}

/// The type at that offset: what kind it is, with the `btf_type_tag`s of its chain in braces,
/// and what it is made from in parentheses. A typedef is its name, a base type its name, and a
/// structure without a name just `struct`.
fn spell(all: &[Entry], offset: Option<u64>) -> String {
    let Some(offset) = offset else { return "void".to_owned() };
    let entry = at(all, offset);
    let tags = chain(all, entry.annotation, "btf_type_tag");
    let tags = if tags.is_empty() { String::new() } else { format!("{{{}}}", tags.join(",")) };
    let name = entry.name.as_deref().unwrap_or("");
    let tagged = |kind: &str| format!("{} {name}", kind).trim_end().to_owned() + &tags;
    let kind = match entry.tag {
        0x24 => return format!("{name}{tags}"),
        0x13 => return tagged("struct"),
        0x17 => return tagged("union"),
        0x04 => return tagged("enum"),
        0x16 => name,
        0x0f => "pointer",
        0x26 => "const",
        0x35 => "volatile",
        0x01 => "array",
        0x15 => "function",
        other => panic!("tag {other:#x} is not one this reader spells"),
    };
    format!("{kind}{tags}({})", spell(all, entry.ty))
}

/// The type of the entry of that name, spelled.
fn type_of(all: &[Entry], name: &str) -> String {
    spell(all, named(all, name).ty)
}

/// How many tags of that name there are.
fn tags_named(all: &[Entry], name: &str) -> usize {
    all.iter().filter(|entry| entry.tag == ANNOTATION && entry.name.as_deref() == Some(name)).count()
}

/// `dwarf-btf-type-tag-1.c`: a tag after the star tags the pointer.
#[test]
fn a_tag_after_the_star_tags_the_pointer() {
    let all = quiet("one", "int * __attribute__((btf_type_tag(\"__user\"))) ptr;\n");
    assert_eq!(counts(&all), (1, 1));
    assert_eq!(type_of(&all, "ptr"), "pointer{__user}(int)");
}

/// `dwarf-btf-type-tag-2.c`: two annotations, one for each string, shared by every type that
/// carries them, with `tag2` chained to `tag1` wherever both are written.
#[test]
fn an_annotation_is_shared_by_every_type_that_carries_it() {
    let source = r#"
#define __tag1 __attribute__((btf_type_tag("tag1")))
#define __tag2 __attribute__((btf_type_tag("tag2")))

int __tag1 foo;
char * __tag1 __tag2 bar;

struct S
{
  unsigned char bytes[8];
  unsigned long __tag1 t;
  void *ptr;
};

struct S * __tag1 __tag2 my_S;
"#;
    let all = quiet("shared", source);
    assert_eq!(counts(&all), (2, 5));
    assert_eq!(type_of(&all, "foo"), "int{tag1}");
    assert_eq!(type_of(&all, "bar"), "pointer{tag2,tag1}(char)");
    assert_eq!(type_of(&all, "bytes"), "array(unsigned char)");
    assert_eq!(type_of(&all, "t"), "long unsigned int{tag1}");
    assert_eq!(type_of(&all, "ptr"), "pointer(void)");
    assert_eq!(type_of(&all, "my_S"), "pointer{tag2,tag1}(struct S)");
}

/// `dwarf-btf-type-tag-3.c`: a tag on a typedef's use tags a copy of the typedef, and one in a
/// typedef tags what it names, under the qualifiers written on its use.
#[test]
fn a_tag_goes_on_the_typedef_or_under_the_qualifiers() {
    let source = r#"
#define __tag1 __attribute__((btf_type_tag ("tag1")))
#define __tag2 __attribute__((btf_type_tag ("tag2")))

typedef const int foo;
typedef int __tag1 bar;

foo __tag2 x;
const bar y;
"#;
    let all = quiet("typedefs", source);
    assert_eq!(counts(&all), (2, 2));
    assert_eq!(type_of(&all, "x"), "foo{tag2}(const(int))");
    assert_eq!(type_of(&all, "y"), "const(bar(int{tag1}))");
}

/// `dwarf-btf-type-tag-4.c`: a tag after the body of a structure, a union or an enumeration
/// tags the type itself, and the three share the one annotation.
#[test]
fn a_tag_after_a_body_tags_the_type_itself() {
    let source = r#"
enum E
{
  ONE,
  TWO
} __attribute__((btf_type_tag("foo")));

enum E some_e;

struct S
{
  int i;
  char c;
} __attribute__((btf_type_tag("foo")));

typedef struct S S1;
S1 plain_s1;
const S1 const_s1;

union U
{
  int x;
  char y;
} __attribute__((btf_type_tag("foo")));

volatile union U volatile_u;
"#;
    let all = quiet("bodies", source);
    assert_eq!(counts(&all), (1, 3));
    assert_eq!(type_of(&all, "some_e"), "enum E{foo}");
    assert_eq!(type_of(&all, "plain_s1"), "S1(struct S{foo})");
    assert_eq!(type_of(&all, "const_s1"), "const(S1(struct S{foo}))");
    assert_eq!(type_of(&all, "volatile_u"), "volatile(union U{foo})");
}

/// `dwarf-btf-type-tag-5.c`: a tag after an array's declarator tags the array.
#[test]
fn a_tag_after_an_array_tags_the_array() {
    let all = quiet("array", "int arr[8] __attribute__((btf_type_tag(\"tagged_arr\")));\n");
    assert_eq!(counts(&all), (1, 1));
    assert_eq!(type_of(&all, "arr"), "array{tagged_arr}(int)");
}

/// `dwarf-btf-type-tag-6.c`: a tag on a structure or a union that is already defined is
/// ignored with gcc's warning, through a typedef too, and nothing is written for it.
///
/// gcc's caret is on the type's name and this compiler's on the attribute. gcc also writes `y`
/// as `const struct foo`, losing the typedef it was declared with, which is kept here.
#[test]
fn a_tag_on_a_defined_record_is_ignored_with_gccs_warning() {
    let source = r#"
struct foo
{
  int a;
  char c;
};

struct foo __attribute__((btf_type_tag ("tag1"))) x;
typedef const struct foo c_foo;
c_foo __attribute__((btf_type_tag ("tag2"))) y;

union bar
{
  int s;
  unsigned int u;
};

typedef union bar __attribute__((btf_type_tag("tag3"))) tag_bar;
const tag_bar z;
"#;
    let (object, said) = build("defined", source);
    let all = entries(&object);
    assert_eq!(counts(&all), (0, 0));
    let warned = [
        "one.c:8:27: warning: ignoring attributes applied to 'struct foo' after definition",
        "one.c:10:22: warning: ignoring attributes applied to 'struct foo' after definition",
        "one.c:18:34: warning: ignoring attributes applied to 'union bar' after definition",
    ];
    for line in warned {
        assert_eq!(said.matches(line).count(), 1, "{line}:\n{said}");
    }
    assert_eq!(said.matches("warning:").count(), 3, "{said}");
    assert_eq!(type_of(&all, "x"), "struct foo");
    assert_eq!(type_of(&all, "y"), "c_foo(const(struct foo))");
    assert_eq!(type_of(&all, "z"), "const(tag_bar(union bar))");
}

/// `dwarf-btf-type-tag-7.c`: a typedef whose use carries a tag of its own is a second typedef
/// entry, whose chain goes on into the tags of the type the typedef names.
#[test]
fn a_tagged_use_of_a_tagged_typedef_is_a_second_typedef() {
    let source = r#"
#define __tag1 __attribute__((btf_type_tag ("tag1")))
#define __tag2 __attribute__((btf_type_tag ("tag2")))

typedef int __tag1 foo;

foo a;
foo __tag2 b;
"#;
    let all = quiet("again", source);
    assert_eq!(counts(&all), (2, 3));
    assert_eq!(all.iter().filter(|entry| entry.tag == 0x16).count(), 2);
    assert_eq!(type_of(&all, "a"), "foo(int{tag1})");
    assert_eq!(type_of(&all, "b"), "foo{tag2,tag1}(int{tag1})");
}

/// `dwarf-btf-type-tag-9.c`: several tags on one type, written in both syntaxes, each chain the
/// last written first, and a chain shared only where it is the same from that tag to its end.
#[test]
fn several_tags_on_one_type_chain_the_last_written_first() {
    let source = r#"
typedef int __attribute__((btf_type_tag ("A"), btf_type_tag ("B"))) myint;

myint x;

struct S
{
  myint * __attribute__((btf_type_tag ("A"))) p;
  unsigned long __attribute__((btf_type_tag ("A")))
                __attribute__((btf_type_tag ("B")))
                __attribute__((btf_type_tag ("C"))) l;
};

char * [[gnu::btf_type_tag ("B"), gnu::btf_type_tag ("C")]] str;

unsigned long [[gnu::btf_type_tag ("B")]]
do_thing (struct S * __attribute__((btf_type_tag ("C"),
                                    btf_type_tag ("B"),
                                    btf_type_tag ("A"))) arg)
{
  return arg->l * 2;
}
"#;
    let all = quiet("stress", source);
    assert_eq!(counts(&all), (8, 11));
    assert_eq!(type_of(&all, "x"), "myint(int{B,A})");
    assert_eq!(type_of(&all, "p"), "pointer{A}(myint(int{B,A}))");
    assert_eq!(type_of(&all, "l"), "long unsigned int{C,B,A}");
    assert_eq!(type_of(&all, "str"), "pointer{C,B}(char)");
    assert_eq!(type_of(&all, "do_thing"), "long unsigned int{B}");
    assert_eq!(type_of(&all, "arg"), "pointer{A,B,C}(struct S)");
}

/// `dwarf-btf-type-tag-10.c`: after the star of a function's declarator, a `btf_type_tag`
/// tags the pointer it returns and a `btf_decl_tag` the function.
#[test]
fn a_function_returns_the_tagged_pointer_and_carries_the_decl_tag() {
    let source = r#"
int * __attribute__((btf_type_tag ("A")))
__attribute__((btf_decl_tag ("decl")))
foo (int *x, int *y)
{
  if (x && y && *x > *y)
    return x;
  return y;
}
"#;
    let all = quiet("function", source);
    assert_eq!(tags_named(&all, "btf_type_tag"), 1);
    assert_eq!(tags_named(&all, "btf_decl_tag"), 1);
    assert_eq!(type_of(&all, "foo"), "pointer{A}(int)");
    assert_eq!(chain(&all, named(&all, "foo").annotation, "btf_decl_tag"), ["decl"]);
}

/// Where gcc 16 puts a tag written in each of the other places a declaration has.
#[test]
fn each_place_a_tag_is_written_tags_the_level_gcc_puts_it_on() {
    let source = r#"
[[gnu::btf_type_tag("L")]] int *lv;
int * [[gnu::btf_type_tag("P")]] pv;
int pw [[gnu::btf_type_tag("N")]];
int *qa[2] __attribute__((btf_type_tag("Q")));
typedef int *ip;
ip __attribute__((btf_type_tag("T"))) tq;
int (*fp)(void) __attribute__((btf_type_tag("F")));
"#;
    let all = quiet("places", source);
    assert_eq!(counts(&all), (6, 6));
    assert_eq!(type_of(&all, "lv"), "pointer{L}(int)");
    assert_eq!(type_of(&all, "pv"), "pointer{P}(int)");
    assert_eq!(type_of(&all, "pw"), "int{N}");
    assert_eq!(type_of(&all, "qa"), "array{Q}(pointer(int))");
    assert_eq!(type_of(&all, "tq"), "ip{T}(pointer(int))");
    assert_eq!(type_of(&all, "fp"), "pointer{F}(function(int))");
}

/// gcc's warning for a tag on a defined type wherever it stands, once for each attribute in each
/// declarator it lands on, and its other warning for one on a function, which is the one it
/// gives when the function returns a structure. A star past the structure is still tagged. gcc
/// writes `v1` as the structure without the typedef it was declared with, which is kept here.
#[test]
fn every_tag_gcc_ignores_is_warned_of_once() {
    let source = r#"#define T(s) __attribute__((btf_type_tag(s)))
typedef struct { int a; } anon;
anon T("a") v1;
enum e { X };
enum e T("e") v2;
struct s { int a; };
struct s T("x") T("y") v3, *v4;
struct s * T("p") v5;
struct s T("b") v6[2];
void f(struct s T("q") arg);
struct t { struct s T("m") mem; int T("i") n; };
struct s T("r") h(void);
struct s T("w") w1, w2;
"#;
    let (object, said) = build("ignored", source);
    let record = "warning: ignoring attributes applied to 'struct s' after definition";
    let warned = [
        "3:6: warning: ignoring attributes applied to 'struct <anonymous>' after definition",
        "5:8: warning: ignoring attributes applied to 'enum e' after definition",
        &format!("7:10: {record}"),
        &format!("7:17: {record}"),
        &format!("10:17: {record}"),
        &format!("11:21: {record}"),
        "12:10: warning: 'btf_type_tag' attribute does not apply to functions",
    ];
    for line in warned {
        assert_eq!(said.matches(&format!("one.c:{line}")).count(), 1, "{line}:\n{said}");
    }
    assert_eq!(said.matches(&format!("one.c:13:10: {record}")).count(), 2, "{said}");
    assert_eq!(said.matches("warning:").count(), 9, "{said}");
    let all = entries(&object);
    assert_eq!(type_of(&all, "v1"), "anon(struct)");
    assert_eq!(type_of(&all, "v3"), "struct s");
    assert_eq!(type_of(&all, "v4"), "pointer{y,x}(struct s)");
    assert_eq!(type_of(&all, "v5"), "pointer{p}(struct s)");
    assert_eq!(type_of(&all, "v6"), "array{b}(struct s)");
}

/// A `btf_decl_tag` after a star is the declaration's, at the end of its chain, where what is
/// inside the star is the name, a function or an array. Inside another star gcc says it does not
/// apply to types.
#[test]
fn a_decl_tag_after_a_star_is_the_declarations() {
    let source = r#"#define D(s) __attribute__((btf_decl_tag(s)))
int D("s1") * D("p1") v D("a1");
int * D("p2") * D("p3") w;
struct st { int * D("m1") mem; };
struct st use;
int f(int * D("q1") arg) { return *arg; }
int * D("x") arr[2];
int (* D("y") fp)(void);
int * D("z") (* D("y") fpr)(void) D("a");
int ** D("k") pp;
"#;
    let (object, said) = build("starred", source);
    let line = "one.c:3:7: warning: 'btf_decl_tag' attribute does not apply to types";
    assert_eq!(said.matches(line).count(), 1, "{said}");
    assert_eq!(said.matches("warning:").count(), 1, "{said}");
    let all = entries(&object);
    let decl = |name: &str| chain(&all, named(&all, name).annotation, "btf_decl_tag");
    assert_eq!(decl("v"), ["s1", "a1", "p1"]);
    assert_eq!(decl("w"), ["p3"]);
    assert_eq!(decl("mem"), ["m1"]);
    assert_eq!(decl("arg"), ["q1"]);
    assert_eq!(decl("arr"), ["x"]);
    assert_eq!(decl("fp"), ["y"]);
    assert_eq!(decl("fpr"), ["a", "y", "z"]);
    assert_eq!(decl("pp"), ["k"]);
}
