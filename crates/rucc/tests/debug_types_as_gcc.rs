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
const DW_TAG_POINTER: u64 = 0x0f;
const DW_TAG_STRUCTURE: u64 = 0x13;
const DW_TAG_SUBROUTINE: u64 = 0x15;
const DW_TAG_TYPEDEF: u64 = 0x16;
const DW_TAG_UNSPECIFIED_PARAMETERS: u64 = 0x18;
const DW_TAG_VARIABLE: u64 = 0x34;
const DW_TAG_SUBRANGE: u64 = 0x21;
const DW_TAG_SUBPROGRAM: u64 = 0x2e;
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
    build_for(TARGET, what, source)
}

/// The same, for another target.
fn build_for(target: &str, what: &str, source: &str) -> Vec<u8> {
    let dir: PathBuf = std::env::temp_dir().join(format!("rucc-dg-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    std::fs::write(dir.join("one.c"), source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([target, "-std=gnu11", "-g", "-O2", "-c", "-o"])
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

/// An array of a typedef name for an array is one entry too, as gcc writes it, and the name is
/// described only where something names it directly.
#[test]
fn an_array_of_a_typedef_name_for_an_array_is_one_entry() {
    let source = "\
typedef int *row[4];
typedef row grid[2];
row m[3];
grid g;
struct s { row r[5]; } v;
int f(void) { return 0; }
";
    let all = entries(&build("rows", source));
    let ranges = |name: &str| {
        let variable = named(&all, DW_TAG_VARIABLE, name).expect("the variable is described");
        let mut array = at(&all, variable.ty.expect("a variable has a type"));
        if array.tag == DW_TAG_TYPEDEF {
            array = at(&all, array.ty.expect("a typedef has a type"));
        }
        assert_eq!(array.tag, DW_TAG_ARRAY, "{name}");
        let element = at(&all, array.ty.expect("an element type"));
        assert_ne!(element.tag, DW_TAG_TYPEDEF, "{name}: the element is not row");
        let ranges = array.children.iter().filter(|&&child| all[child].tag == DW_TAG_SUBRANGE);
        ranges.count()
    };
    assert_eq!((ranges("m"), ranges("g")), (2, 2));
    let r = named(&all, DW_TAG_MEMBER, "r").expect("r is described");
    let array = at(&all, r.ty.expect("a member has a type"));
    let ranges = array.children.iter().filter(|&&child| all[child].tag == DW_TAG_SUBRANGE);
    assert_eq!((array.tag, ranges.count()), (DW_TAG_ARRAY, 2));
    assert!(named(&all, DW_TAG_TYPEDEF, "row").is_none(), "nothing names row directly");
    assert!(named(&all, DW_TAG_TYPEDEF, "grid").is_some());
}

/// A file with variables and no code still describes them, as gcc does: the kernel has tables of
/// structures in files like that, and their types reach the BTF only through these entries.
#[test]
fn a_file_of_only_variables_still_describes_them() {
    let source =
        "struct op { int code; const char *name; };\nconst struct op ops[2] = { { 1, \"a\" } };\n";
    let all = entries(&build("data", source));
    let ops = named(&all, DW_TAG_VARIABLE, "ops").expect("ops is described");
    let readonly = at(&all, ops.ty.expect("ops has a type"));
    assert_eq!(readonly.tag, DW_TAG_CONST);
    let array = at(&all, readonly.ty.expect("the const has a type"));
    assert_eq!(array.tag, DW_TAG_ARRAY);
    assert!(named(&all, DW_TAG_STRUCTURE, "op").is_some());
}

/// `const char name[16]` as a member is a const over the array of const char, since gcc writes the
/// member as read-only on top of its elements. A pointer to such an array and the entry of a typedef
/// name for one stay bare, and a member written with the name has the const over the name.
#[test]
fn an_array_of_const_elements_is_const_itself_where_it_is_declared() {
    let source = "\
typedef const char name_t[8];
struct s { const char a[4]; const char *h[2]; const char (*p)[4]; name_t n; } v;
int f(void) { return 0; }
";
    let all = entries(&build("readonly", source));
    let member = |name: &str| {
        let member = named(&all, DW_TAG_MEMBER, name).expect("the member is described");
        at(&all, member.ty.expect("a member has a type"))
    };
    let a = member("a");
    assert_eq!(a.tag, DW_TAG_CONST);
    let array = at(&all, a.ty.expect("the const has a type"));
    assert_eq!(array.tag, DW_TAG_ARRAY);
    assert_eq!(at(&all, array.ty.expect("an element type")).tag, DW_TAG_CONST);
    assert_eq!(member("h").tag, DW_TAG_ARRAY, "the elements are not const, what they point at is");
    let pointer = member("p");
    assert_eq!(at(&all, pointer.ty.expect("a pointer has a type")).tag, DW_TAG_ARRAY);
    let n = member("n");
    assert_eq!(n.tag, DW_TAG_CONST);
    let name = at(&all, n.ty.expect("the const has a type"));
    assert_eq!((name.tag, name.name.as_deref()), (DW_TAG_TYPEDEF, Some("name_t")));
    assert_eq!(at(&all, name.ty.expect("name_t has a type")).tag, DW_TAG_ARRAY);
}

/// A function is described the way its definition writes it: the return type as the definition
/// spells it, whatever a prototype above said, and a parameter named like its typedef still
/// typed with the typedef.
#[test]
fn a_function_is_described_as_its_definition_writes_it() {
    let source = "\
typedef unsigned int u32;
typedef unsigned int phandle;
typedef struct pts { int x; } poll_table;
u32 f(int a);
unsigned int f(int b) { return b; }
int g(phandle phandle) { return phandle; }
int h(poll_table *poll_table) { return poll_table->x; }
";
    let all = entries(&build("definition", source));
    let function = |name: &str| named(&all, DW_TAG_SUBPROGRAM, name).expect("it is described");
    let parameter = |name: &str| {
        let param = function(name)
            .children
            .iter()
            .map(|&child| &all[child])
            .find(|child| child.tag == DW_TAG_FORMAL_PARAMETER)
            .expect("the function takes a parameter");
        at(&all, param.ty.expect("a parameter has a type"))
    };
    let returns = at(&all, function("f").ty.expect("f returns a value"));
    assert_eq!((returns.tag, returns.name.as_deref()), (DW_TAG_BASE, Some("unsigned int")));
    let g = parameter("g");
    assert_eq!((g.tag, g.name.as_deref()), (DW_TAG_TYPEDEF, Some("phandle")));
    let pointee = at(&all, parameter("h").ty.expect("a pointer has a type"));
    assert_eq!((pointee.tag, pointee.name.as_deref()), (DW_TAG_TYPEDEF, Some("poll_table")));
}

/// A declaration after a function's definition says how the function is described, the way gcc
/// takes the last one, and `typeof` of the function writes its return type the way the first
/// declaration did. `EXPORT_SYMBOL` is such a declaration, which is why the kernel's
/// `bit_waitqueue` returns what its prototype says rather than what its definition says.
#[test]
fn a_function_declared_again_after_its_definition_is_described_as_the_last_declaration_says() {
    let source = "\
typedef unsigned int __u32;
typedef __u32 u32;
typedef unsigned int x32;
__u32 f(int a);
u32 f(int b) { return b; }
extern typeof(f) f;
__u32 g(int a);
x32 g(int b) { return b; }
u32 g(int c);
";
    let all = entries(&build("redeclared", source));
    let returns = |name: &str| {
        let function = named(&all, DW_TAG_SUBPROGRAM, name).expect("it is described");
        let ty = at(&all, function.ty.expect("it returns a value"));
        (ty.tag, ty.name.clone())
    };
    assert_eq!(returns("f"), (DW_TAG_TYPEDEF, Some("__u32".to_owned())));
    assert_eq!(returns("g"), (DW_TAG_TYPEDEF, Some("u32".to_owned())));
}

/// A type a function or a variable uses is described with it even where no signature, member or
/// variable has that type, which is gcc's rule and so what the kernel's BTF has: the enumeration of
/// an enumerator, a type cast to or made a compound literal of, a local nothing reads, and the
/// `__builtin_va_list` a list is declared with. An enumerator in the initializer of a `static`
/// nothing reads goes with it, since the variable is gone.
#[test]
fn a_type_a_function_uses_is_described_though_nothing_names_it() {
    let source = "\
enum in_func { IF_A, IF_B };
enum in_case { IC_A, IC_B };
enum in_global { IG_A, IG_B };
enum in_attr { IA = 8 };
enum dropped { DR_A };
struct in_literal { int a; };
struct never_read { int a; };
typedef struct in_cast { int q; } in_cast_t;
int g = IG_B;
static int unread = DR_A;
int al __attribute__((aligned(IA)));
int f(int v, void *p) {
  struct never_read n;
  switch (v) { case IC_B: return 3; }
  return IF_B + ((in_cast_t *)p)->q + (struct in_literal){ v }.a;
}
int va(int n, ...) { __builtin_va_list ap; __builtin_va_start(ap, n); int r = __builtin_va_arg(ap, int); __builtin_va_end(ap); return r; }
";
    let all = entries(&build("used", source));
    for name in ["in_func", "in_case", "in_global", "in_attr"] {
        assert!(named(&all, DW_TAG_ENUMERATION, name).is_some(), "{name} is described");
    }
    assert!(named(&all, DW_TAG_ENUMERATION, "dropped").is_none(), "dropped goes with unread");
    for name in ["in_literal", "never_read", "in_cast"] {
        assert!(named(&all, DW_TAG_STRUCTURE, name).is_some(), "{name} is described");
    }
    let cast = named(&all, DW_TAG_TYPEDEF, "in_cast_t").expect("the cast's typedef is described");
    assert_eq!(at(&all, cast.ty.expect("in_cast_t has a type")).tag, DW_TAG_STRUCTURE);
    // The AAPCS64 list is a structure with a tag, which gcc describes with no typedef name over it.
    assert!(named(&all, DW_TAG_STRUCTURE, "__va_list").is_some(), "the list is described");
    assert!(named(&all, DW_TAG_TYPEDEF, "__builtin_va_list").is_none());
}

/// An enumerator used outside a function keeps its enumeration with the next file-scope variable
/// the unit defines, past an `extern` declaration and a function with locals between them, as gcc
/// does. A `static` nothing reads takes it and keeps nothing.
#[test]
fn what_was_used_outside_a_function_waits_for_the_next_variable_the_unit_defines() {
    let source = "\
enum hooks { H_IN, H_COUNT };
struct table { int x[H_COUNT]; };
extern int elsewhere;
static inline int unused(void) { int local = 0; static int counted; return local + counted++; }
int kept;
enum lost { L_A, L_COUNT };
struct other { int y[L_COUNT]; };
static int unread;
int f(void) { return kept; }
";
    let all = entries(&build("waiting-uses", source));
    assert!(named(&all, DW_TAG_ENUMERATION, "hooks").is_some());
    assert!(named(&all, DW_TAG_ENUMERATION, "lost").is_none());
}

/// A `static inline` the code calls is described even where every copy of it folded away, with
/// what it takes and gives back and the enumerators it uses, as gcc does. One nothing calls is
/// not.
#[test]
fn an_inline_function_folded_away_is_still_described() {
    let source = "\
enum kind { K_A, K_B };
enum mode { M_X, M_Y };
enum unseen { U_A };
struct only_param { int a; };
static inline enum kind kind_of(int flags, struct only_param *p) { return flags & 1 ? K_A : K_B; }
static inline int moded(int x) { return x == M_Y; }
static inline int unused_inline(enum unseen u) { return u; }
int f(void) { return kind_of(2, 0) + moded(0); }
";
    let all = entries(&build("folded-inline", source));
    for name in ["kind_of", "moded"] {
        let one = named(&all, DW_TAG_SUBPROGRAM, name).expect("the inline is described");
        assert!(!one.declaration, "{name}");
    }
    assert!(named(&all, DW_TAG_ENUMERATION, "kind").is_some(), "what kind_of gives back");
    assert!(named(&all, DW_TAG_STRUCTURE, "only_param").is_some(), "what kind_of takes");
    assert!(named(&all, DW_TAG_ENUMERATION, "mode").is_some(), "what moded uses");
    assert!(named(&all, DW_TAG_SUBPROGRAM, "unused_inline").is_none());
    assert!(named(&all, DW_TAG_ENUMERATION, "unseen").is_none());
}

/// A function or variable another unit defines has an entry that says what it is wherever the code
/// the object holds names it, a call in a `static inline` that is called and an address in the data
/// among them, and a call the optimizer folded away as well. None where only an uncalled `static
/// inline` does, or an arm a constant condition never takes. What it takes and returns are
/// described with it, as with gcc, which puts their types in the kernel's BTF.
#[test]
fn a_function_or_variable_another_unit_defines_is_declared_where_referred_to() {
    let source = "\
typedef unsigned int u32;
struct a1 { int a; }; struct a3 { int a; }; struct a4 { int a; };
struct a5 { int a; }; struct a6 { int a; };
void addr_taken(struct a1 *);
void folded_away(void);
void never_taken(void);
void called_in_inline(struct a4 *);
void called_in_dead_inline(struct a3 *);
extern struct a5 ext_read;
extern struct a6 ext_unread;
long spelled(const u32 x, ...);
static inline void inl(void *p) { called_in_inline(p); }
static inline void dead(void *p) { called_in_dead_inline(p); }
static inline void maybe(int x) { if (x) folded_away(); }
void *fp = addr_taken;
int f(void *p) { inl(p); spelled(1); maybe(0); if (sizeof(int) == 2) never_taken(); return ext_read.a; }
";
    let all = entries(&build("declared", source));
    let declared = |tag: u64, name: &str| named(&all, tag, name).filter(|one| one.declaration);
    let params = |which: &Entry| -> Vec<&Entry> {
        which.children.iter().map(|&child| &all[child]).collect()
    };
    let taken = declared(DW_TAG_SUBPROGRAM, "addr_taken").expect("addr_taken is declared");
    let [param] = params(taken)[..] else { panic!("{:?}", params(taken)) };
    assert_eq!(param.tag, DW_TAG_FORMAL_PARAMETER);
    let pointer = at(&all, param.ty.expect("the parameter has a type"));
    assert_eq!(at(&all, pointer.ty.expect("a pointer to a1")).name.as_deref(), Some("a1"));
    assert!(declared(DW_TAG_SUBPROGRAM, "called_in_inline").is_some());
    assert!(named(&all, DW_TAG_STRUCTURE, "a4").is_some(), "what the inline's callee takes");
    assert!(named(&all, DW_TAG_SUBPROGRAM, "called_in_dead_inline").is_none());
    assert!(named(&all, DW_TAG_STRUCTURE, "a3").is_none(), "only the dead inline names a3");
    assert!(declared(DW_TAG_SUBPROGRAM, "folded_away").is_some(), "the call is in the source");
    assert!(named(&all, DW_TAG_SUBPROGRAM, "never_taken").is_none());
    let read = declared(DW_TAG_VARIABLE, "ext_read").expect("ext_read is declared");
    assert_eq!(at(&all, read.ty.expect("ext_read has a type")).name.as_deref(), Some("a5"));
    assert!(named(&all, DW_TAG_VARIABLE, "ext_unread").is_none());
    assert!(named(&all, DW_TAG_STRUCTURE, "a6").is_none());
    let spelled = declared(DW_TAG_SUBPROGRAM, "spelled").expect("spelled is declared");
    let returns = at(&all, spelled.ty.expect("spelled returns long"));
    assert_eq!(returns.name.as_deref(), Some("long int"));
    let [first, rest] = params(spelled)[..] else { panic!("{:?}", params(spelled)) };
    let constant = at(&all, first.ty.expect("the parameter has a type"));
    assert_eq!(constant.tag, DW_TAG_CONST);
    let name = at(&all, constant.ty.expect("const over u32"));
    assert_eq!((name.tag, name.name.as_deref()), (DW_TAG_TYPEDEF, Some("u32")));
    assert_eq!(rest.tag, DW_TAG_UNSPECIFIED_PARAMETERS);
}

/// A pointer into an address space is described as the pointer it is. The kernel's `void __percpu *`
/// is `void __seg_gs *` on x86, and a member of that type used to have no entry at all, which left a
/// hole in the record pahole reads.
#[test]
fn a_pointer_into_an_address_space_is_described() {
    let source = "\
struct s { void __seg_gs *p; const void __seg_gs *c; int __seg_gs *q; } v;
extern void free_percpu(void __seg_gs *);
void f(void) { free_percpu(v.p); }
";
    let all = entries(&build_for("--target=x86_64-unknown-linux-gnu", "space", source));
    let record = named(&all, DW_TAG_STRUCTURE, "s").expect("s is described");
    let members: Vec<_> = record.children.iter().map(|&child| all[child].name.clone()).collect();
    assert_eq!(members, [Some("p".to_owned()), Some("c".to_owned()), Some("q".to_owned())]);
    let p = named(&all, DW_TAG_MEMBER, "p").expect("p is a member");
    let pointer = at(&all, p.ty.expect("p has a type"));
    assert_eq!((pointer.tag, pointer.ty), (DW_TAG_POINTER, None), "a pointer to void");
    let free = named(&all, DW_TAG_SUBPROGRAM, "free_percpu").expect("free_percpu is declared");
    assert_eq!(free.children.len(), 1, "it takes the pointer");
}

/// A `static` the back end dropped because nothing reads it still has an entry, with its type and
/// no location, as gcc writes it. That is how `exitcall_t` gets into a module with a `module_exit`,
/// from the pointer the macro puts in `.exitcall.exit` and marks unused. A `static` that is only
/// declared, a local and a name another unit defines get no such entry.
#[test]
fn a_static_nothing_reads_is_still_described() {
    let source = "\
struct quiet { int a; };
struct local_only { int a; };
typedef void (*exit_t)(void);
static void bye(void) {}
static exit_t on_exit __attribute__((unused)) = bye;
static struct quiet hushed;
static const int seven = 7;
extern struct quiet elsewhere;
static inline int never(void) { static struct local_only kept; return kept.a; }
int f(void) { return seven; }
";
    let all = entries(&build("dropped-static", source));
    for name in ["on_exit", "hushed", "seven"] {
        let one = named(&all, DW_TAG_VARIABLE, name).expect("the static is described");
        assert!(!one.declaration, "{name}");
    }
    assert!(named(&all, DW_TAG_TYPEDEF, "exit_t").is_some(), "the type of on_exit");
    assert!(named(&all, DW_TAG_STRUCTURE, "quiet").is_some(), "the type of hushed");
    assert!(named(&all, DW_TAG_VARIABLE, "elsewhere").is_none());
    assert!(named(&all, DW_TAG_VARIABLE, "kept").is_none());
    assert!(named(&all, DW_TAG_STRUCTURE, "local_only").is_none());
}

/// A call after a statement control never comes out of has no declaration, since gcc drops those
/// statements before it looks at the calls. A `return`, an `if (!0) return;`, a `break`, a call to
/// a `noreturn` function and an `if` whose arms both return are such statements. A call a label
/// makes reachable again, or one a loop comes back round to, is declared as usual.
#[test]
fn a_call_control_never_reaches_is_not_declared() {
    let source = "\
extern void after_return(void);
extern void after_true_if(void);
extern void after_label(void);
extern void after_break(void);
extern void in_loop_tail(void);
extern void after_noreturn(void);
extern void after_bug(void);
extern void kept(void);
extern void after_both(int);
extern void bye(void) __attribute__((noreturn));
void a(void) { kept(); return; after_return(); }
void b(void) { if (!0) { return; } after_true_if(); }
void c(int x) { if (x) goto out; return; out: after_label(); }
void d(int x) { while (x) { break; after_break(); } for (;;) { if (x) break; in_loop_tail(); } }
void e(void) { bye(); after_noreturn(); }
void f(int x) { if (x) return; else return; after_both(x); }
void g(void) { do { __builtin_unreachable(); } while (0); after_bug(); }
";
    let all = entries(&build("never-reached", source));
    for name in ["kept", "after_label", "in_loop_tail", "bye"] {
        assert!(named(&all, DW_TAG_SUBPROGRAM, name).is_some(), "{name} is declared");
    }
    for name in [
        "after_return",
        "after_true_if",
        "after_break",
        "after_noreturn",
        "after_both",
        "after_bug",
    ] {
        assert!(named(&all, DW_TAG_SUBPROGRAM, name).is_none(), "{name} is not");
    }
}

/// A `va_list` is a typedef of `__builtin_va_list` where the list has no name of its own, an array
/// on x86-64 and a pointer on i386, and a typedef of `struct __va_list` on AArch64, as gcc writes
/// both. A `typedef __builtin_va_list` there was a type only rucc's BTF had.
#[test]
fn a_va_list_on_aarch64_names_the_structure_and_elsewhere_the_builtin() {
    let source = "\
typedef __builtin_va_list va_list;
int vf(const char *f, va_list ap);
int g(const char *f, ...) { va_list ap; __builtin_va_start(ap, f); int r = vf(f, ap); __builtin_va_end(ap); return r; }
";
    let all = entries(&build("va-list", source));
    let list = named(&all, DW_TAG_TYPEDEF, "va_list").expect("va_list is described");
    let under = at(&all, list.ty.expect("va_list has a type"));
    assert_eq!((under.tag, under.name.as_deref()), (DW_TAG_STRUCTURE, Some("__va_list")));
    assert!(named(&all, DW_TAG_TYPEDEF, "__builtin_va_list").is_none());
    let all = entries(&build_for("--target=x86_64-unknown-linux-gnu", "va-list", source));
    let list = named(&all, DW_TAG_TYPEDEF, "va_list").expect("va_list is described");
    let under = at(&all, list.ty.expect("va_list has a type"));
    assert_eq!((under.tag, under.name.as_deref()), (DW_TAG_TYPEDEF, Some("__builtin_va_list")));
}

/// A global register variable is described, and the types used outside a function before it go
/// with it, as they would with any variable the unit defines. The arm64 kernel declares
/// `current_stack_pointer` that way after `node_states[NR_NODE_STATES]`, and gcc describes
/// `enum node_states` because of it. An enumeration used after the last variable is lost, as gcc
/// loses it.
#[test]
fn a_global_register_variable_takes_the_types_used_before_it() {
    let source = "\
enum ea { A0, NA };
extern int va[NA];
register unsigned long csp asm (\"sp\");
enum eb { B0, NB };
extern int vb[NB];
";
    let all = entries(&build("register", source));
    assert!(named(&all, DW_TAG_VARIABLE, "csp").is_some());
    assert!(named(&all, DW_TAG_ENUMERATION, "ea").is_some());
    assert!(named(&all, DW_TAG_ENUMERATION, "eb").is_none());
}
