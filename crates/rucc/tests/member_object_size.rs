//! What `__builtin_object_size` says about a member once the function asking has been inlined.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! The kernel's fortified `strcpy` asks `__builtin_object_size (p, 1)` about its own parameter, so
//! the question is only answered after the call is inlined, and by then the address is an add to
//! the start of the structure. Lowering marks that add with the size of the member it steps to,
//! and the answer for the closest member is that size, as it is from gcc. Before the mark the
//! answer was what was left of the whole structure, and `lib/test_fortify` builds that the kernel
//! expects to be refused went through.
//!
//! A member that may run past its end, an array at the end of a structure or a record that ends in
//! a flexible array, is answered as the whole object even where the address it is in was worked
//! out from another member. bcachefs's `bkey_xattr_init` takes `container_of` on `&_k->k` and
//! clears the `v` after it, and when the answer was what was left of `k`, which is nothing, the
//! fortified `memset` refused the build. pm8001 casts the address of a pointer member to a structure
//! and copies into the flexible array at its end, which is the same question.
//!
//! An array at the end of a record is only that open where the record is what a pointer points
//! at. mwifiex's `mwifiex_cmd_802_11_key_material_v1` casts `&cmd->params.key_material` to a layout
//! that ends in an array of four and clears the array, and when the answer was what was left of
//! the member under the cast, the fortified `memset` refused the build. A pointer that is a step
//! into a local has the array's own length, and an array in a record that is a member of another
//! one has its own length wherever that is. An array with no length in a declared object is the
//! rest of that object, past the record holding it, at either kind.
//!
//! How far a trailing array may run is what `-fstrict-flex-arrays` says. At level three only `[]`
//! runs past its end, so the `bits` of a `struct cpumask` is its 64 bytes, and the kernel's
//! fortified `bitmap_copy` checks the copy against that. A member taken from an element whose index
//! is only known when the program runs is as long as its type says too, whichever element it is in.
//!
//! Every answer below is what gcc 16 gives at `-O2` for the same source. Each one is checked by a
//! call to a function declared with `error`, under a test the right answer makes false, so a
//! wrong answer is a refused build that names it.

use std::path::PathBuf;
use std::process::{Command, Output};

const SOURCE: &str = r#"
typedef unsigned long size_t;
struct in { int x; char y[8]; };
struct o { int a; char buf[16]; struct in inner; int c; char tail[4]; } inst;
struct o *gp;
struct fl { int n; char d[]; };
struct mid { long a; struct fl in; };
struct holder { int x; struct mid m; } *hp;
struct key { long a, b, c; char t; };
struct ki { struct key k; long v[]; } *kp;
struct fx { char t, n; short l; char name[]; };
struct kx { union { struct key k; struct ki k_i; }; struct fx v; };
struct payload { int sig; short id; long *specific; };
struct control { int ret, len; char buffer[]; };
struct ks { short t, l, id, info, len; char key[50]; } __attribute__((packed));
struct km { short action; struct ks set; } __attribute__((packed));
struct kw { short action; struct ks set[4]; } __attribute__((packed));
struct cmd { short c, s; union { struct km km; char big[300]; } p; } __attribute__((packed)) *cp;
struct out2 { int a; struct kw in; } *op;
struct outu { int a; union { struct kw in; char big[300]; } u; int z; } *up;
struct pad { int a; char c; char f[]; };
struct opad { int x; struct pad in; };
struct npad { int x; struct pad in; int z; };
extern void *grab(size_t) __attribute__((alloc_size(1)));
#define container_of(p, T, m) ((T *)((char *)(p) - __builtin_offsetof(T, m)))
#define I static inline __attribute__((always_inline))
I size_t q1(const void *p) { return __builtin_object_size(p, 1); }
I size_t q3(const void *p) { return __builtin_object_size(p, 3); }
#define WANT(name, got, want) \
    do { \
        extern void name(void) __attribute__((error(#name))); \
        if ((got) != (size_t)(want)) name(); \
    } while (0)
void f(int i) {
    WANT(wrong_buf, q1(inst.buf), 16);
    WANT(wrong_index, q1(&inst.buf[2]), 14);
    WANT(wrong_cast, q1((char *)&inst.inner + 6), 6);
    WANT(wrong_first, q1(&inst.a), 4);
    WANT(wrong_arrow, q1(gp->buf), 16);
    WANT(wrong_tail, q1(gp->tail), -1);
    WANT(wrong_inner, q1(&gp->inner), 12);
    WANT(wrong_nested, q1(gp->inner.y), 8);
    WANT(wrong_past, q1((char *)gp->buf + 20), 0);
    WANT(wrong_least, q3(inst.buf), 16);
    WANT(wrong_least_arrow, q3(gp->buf), 16);
    WANT(wrong_unknown, q1(&inst.buf[i]), -1);
    WANT(wrong_whole, q1(&inst), 40);
    char *p = inst.buf;
    WANT(wrong_local, q1(p + 4), 12);
    WANT(wrong_flex_holder, q1(&hp->m), -1);
    WANT(wrong_flex_inner, q1(&hp->m.in), -1);
    int k = 1;
    WANT(wrong_counted, q1(gp->buf + k), 16);
    WANT(wrong_counted_object, q1(inst.buf + k), 16);
    WANT(wrong_counted_whole, q1((char *)&inst + 30 + k), 9);
    struct kx *open = container_of(&kp->k, struct kx, k);
    WANT(wrong_open_unknown, q1(&open->v), -1);
    struct ki *heap = grab(64);
    struct kx *held = container_of(&heap->k, struct kx, k);
    WANT(wrong_open_heap, q1(&held->v), 32);
    WANT(wrong_open_name, q1(held->v.name), 28);
    struct payload *pay = grab(128);
    struct control *ctl = (struct control *)&pay->specific;
    WANT(wrong_cast_flex, q1(ctl->buffer), 112);
    struct kw *w = (struct kw *)&cp->p.km;
    WANT(wrong_trailing_cast, q1(w->set), -1);
    struct cmd cmd_local;
    struct kw *x = (struct kw *)&cmd_local.p.km;
    WANT(wrong_trailing_local, q1(x->set), 240);
    WANT(wrong_trailing_union, q1(up->u.in.set), 240);
    WANT(wrong_trailing_nested, q1(op->in.set), 240);
    struct kw *ip = &op->in;
    WANT(wrong_trailing_two, q1(ip->set), -1);
    struct opad opad_local;
    WANT(wrong_trailing_padding, q1(opad_local.in.f), 3);
    struct opad *po = &opad_local;
    WANT(wrong_trailing_padding_local, q1(po->in.f), 3);
    struct npad npad_local;
    WANT(wrong_flex_rest, q1(npad_local.in.f), 7);
    WANT(wrong_flex_rest_now, __builtin_object_size(npad_local.in.f, 1), 7);
    WANT(wrong_flex_rest_index, __builtin_object_size(&npad_local.in.f[1], 1), 6);
    WANT(wrong_flex_rest_whole, __builtin_object_size(opad_local.in.f, 0), 3);
}
"#;

/// The kernel's `write_overflow-strncpy.c`, cut down to the fortified `strncpy` it calls.
const OVERFLOW: &str = r#"
typedef unsigned long size_t;
extern void __write_overflow(void)
    __attribute__((__error__("detected write beyond size of object (1st parameter)")));
extern void fortify_panic(void) __attribute__((noreturn));
extern inline __attribute__((always_inline)) __attribute__((gnu_inline))
char *strncpy(char *const p, const char *q, size_t size) {
    const size_t p_size = __builtin_object_size(p, 1);
    if (__builtin_constant_p(p_size < size) && p_size < size)
        __write_overflow();
    if (p_size < size)
        fortify_panic();
    return __builtin_strncpy(p, q, size);
}
struct fortify_object { int a; char buf[16]; int c; } instance;
const char large_src[32] = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
void f(void) { strncpy(instance.buf, large_src, sizeof(instance.buf) + 1); }
"#;

/// Trailing arrays of a fixed length, asked about through a parameter, a local and a variable
/// subscript. `STRICT` is one when the build passes `-fstrict-flex-arrays=3`.
const TRAILING: &str = r#"
typedef unsigned long size_t;
struct m { unsigned long bits[8]; };
struct n { long x; unsigned long bits[8]; };
struct topo { int id; struct m core_sibling; struct m llc_sibling; } def[4];
#define I static inline __attribute__((always_inline))
I size_t d1(const void *p) { return __builtin_dynamic_object_size(p, 1); }
#define WANT(name, got, want) \
    do { \
        extern void name(void) __attribute__((error(#name))); \
        if ((got) != (size_t)(want)) name(); \
    } while (0)
void f(struct m *mp, struct n *np, int i) {
    WANT(wrong_strict_whole, d1(mp->bits), STRICT ? 64 : -1);
    WANT(wrong_strict_param, d1(np->bits), STRICT ? 64 : -1);
    struct n *local = np;
    WANT(wrong_strict_local, d1(local->bits), STRICT ? 64 : -1);
    WANT(wrong_strict_index, d1(&np->bits[1]), STRICT ? 56 : -1);
    WANT(wrong_variable, d1(def[i].core_sibling.bits), 64);
    WANT(wrong_variable_direct, __builtin_dynamic_object_size(def[i].core_sibling.bits, 1), 64);
}
"#;

/// A member reached at an index a branch has bounded, the way acpi's `flatten_lpi_states` takes
/// `&pr->power.lpi_states[flat_state_cnt]` once it has given up at eight states. At least one
/// element of the array is left there, and gcc 16 folds the fortified `memcpy`'s test on that.
const BOUNDED: &str = r#"
typedef unsigned long size_t;
struct state { char b[72]; };
struct power { int count; union { int c[4]; struct state s[8]; }; };
struct proc { long id; struct power power; } *pp;
void f(unsigned i) {
    extern void wrong_bounded(void) __attribute__((error("wrong_bounded")));
    if (i >= 8)
        return;
    if (__builtin_dynamic_object_size(&pp->power.s[i], 1) < sizeof(struct state))
        wrong_bounded();
}
"#;

/// An array in a union, which is as long as its type says unless the union is what the pointer
/// points at. crypto/vmac's `vmac_update` copies into `&dctx->partial[dctx->partial_size]`, where
/// `partial` shares a union with the words it is read as, and gcc checks the copy against what is
/// left of the 128 bytes of `partial`.
const UNION: &str = r#"
typedef unsigned long size_t;
struct ctx { union { unsigned char partial[128]; unsigned long words[16]; }; unsigned int size; };
union both { unsigned char bytes[16]; unsigned long words[2]; };
#define I static inline __attribute__((always_inline))
I size_t d1(const void *p) { return __builtin_dynamic_object_size(p, 1); }
#define WANT(name, got, want) \
    do { \
        extern void name(void) __attribute__((error(#name))); \
        if ((got) != (size_t)(want)) name(); \
    } while (0)
void f(struct ctx *c, union both *u, unsigned i) {
    WANT(wrong_union_array, d1(c->partial), 128);
    WANT(wrong_union_array_at, d1(&c->partial[4]), 124);
    WANT(wrong_union_words, d1(c->words), 128);
    WANT(wrong_pointed_union, d1(u->bytes), -1);
    if (i < 128 && d1(&c->partial[i]) > 128) {
        extern void wrong_union_index(void) __attribute__((error("wrong_union_index")));
        wrong_union_index();
    }
}
"#;

/// A member of what a pointer points at, which gcc answers with what is left of the member or of
/// the whole object, whichever is less. cxgb4's `t4_alloc_mac_filt` copies each address into
/// `p->macaddr` at a `p` its loop moves through a command on the stack, and gcc works the answer
/// out when the program runs and keeps the field spanning WARN that checks the copy. A parameter
/// whose object nothing says is the member, and so is a member of a local named as it is.
const CAPPED: &str = r#"
typedef unsigned long size_t;
struct big { int x; char name[16]; int y; };
struct ex { unsigned short v; unsigned char mac[6]; };
struct cmd { int a, b; union { struct ex exact[7]; long h; } u; };
extern void *grab(size_t) __attribute__((alloc_size(1)));
extern void take(const void *);
#define D1(p) __builtin_dynamic_object_size(p, 1)
#define WANT(name, got, want) \
    do { \
        extern void name(void) __attribute__((error(#name))); \
        if ((got) != (size_t)(want)) name(); \
    } while (0)
#define RUNS(name, got) \
    do { \
        extern void name(void) __attribute__((error(#name))); \
        if (__builtin_constant_p(got)) name(); \
    } while (0)
void f(size_t n, struct big *param, int count) {
    struct big *c = grab(10);
    WANT(wrong_small, D1(c->name), 6);
    WANT(wrong_small_static, __builtin_object_size(c->name, 1), 6);
    WANT(wrong_param, D1(param->name), 16);
    struct big *b = grab(64);
    WANT(wrong_room, D1(b->name), 16);
    struct big *r = grab(n);
    RUNS(folded_alloc, D1(r->name));
    struct cmd cmd;
    struct ex *p = cmd.u.exact;
    for (int i = 0; i < count; i++, p++)
        RUNS(folded_loop, D1(p->mac));
    struct big local;
    WANT(wrong_local, D1(local.name), 16);
    take(c); take(b); take(r); take(&cmd); take(&local);
}
"#;

const STORED: &str = r#"
typedef unsigned long size_t;
struct inode { long a; char *link; long size; };
struct info { union { int data[15]; char sym[60]; } u; int x; struct inode vfs; };
struct pair { char *one; char *two; };
extern void touch(void);
#define container_of(p, T, m) ((T *)((char *)(p) - __builtin_offsetof(T, m)))
#define D1(p) __builtin_dynamic_object_size(p, 1)
#define WANT(name, got, want) \
    do { \
        extern void name(void) __attribute__((error(#name))); \
        if ((got) != (size_t)(want)) name(); \
    } while (0)
void f(struct inode *inode, struct pair *pair, char *other) {
    inode->link = container_of(inode, struct info, vfs)->u.sym;
    WANT(wrong_stored, D1(inode->link), 60);
    WANT(wrong_stored_static, __builtin_object_size(inode->link, 1), -1);
    pair->one = container_of(inode, struct info, vfs)->u.sym;
    pair->two = other;
    WANT(wrong_beside, D1(pair->one), 60);
    touch();
    WANT(wrong_call, D1(pair->one), -1);
}
"#;

const IN_LOCAL: &str = r#"
typedef unsigned long size_t;
struct block { unsigned char bitmap[27]; unsigned char reserved[3]; unsigned char dentry[2140]; };
struct ptrs { void *in; int max, nr; void *bitmap; void *dentry; };
static inline void make(struct ptrs *d, struct block *b) {
    d->max = 214;
    d->bitmap = b->bitmap;
    d->dentry = b->dentry;
}
extern struct block *get(void);
#define WANT(name, got, want) \
    do { \
        extern void name(void) __attribute__((error(#name))); \
        if ((got) != (size_t)(want)) name(); \
    } while (0)
void f(void) {
    struct ptrs dst;
    make(&dst, get());
    WANT(wrong_inlined, __builtin_dynamic_object_size(dst.bitmap, 1), 27);
    WANT(wrong_inlined_static, __builtin_object_size(dst.bitmap, 1), 27);
}
void g(struct block *b) {
    struct ptrs dst;
    dst.bitmap = b->bitmap;
    dst.dentry = b->dentry;
    WANT(wrong_by_hand, __builtin_dynamic_object_size(dst.bitmap, 1), 27);
}
"#;

const RELEASE: &str = r#"
typedef unsigned long size_t;
struct sge { int a, b; long c; };
struct isgl { unsigned short n; unsigned r2; struct sge sge[]; };
struct recv { unsigned char op, len16; unsigned short wrid; unsigned r2; struct isgl isgl; };
union first { struct recv recv; long flits[16]; };
union last { long flits[16]; struct recv recv; };
union scalar { struct recv recv; int x; };
struct a { long id; union first w; };
struct b { long id; union last w; char x; };
struct c { long id; union scalar w; };
struct d { long id; struct recv r; char x; };
#define WANT(name, got, want) \
    do { \
        extern void name(void) __attribute__((error(#name))); \
        if ((got) != (size_t)(want)) name(); \
    } while (0)
#if __GNUC__ < 14
#define FLEX(gcc13, gcc14, gcc15) (gcc13)
#elif __GNUC__ < 15
#define FLEX(gcc13, gcc14, gcc15) (gcc14)
#else
#define FLEX(gcc13, gcc14, gcc15) (gcc15)
#endif
void f(struct a *a, struct b *b, struct c *c, struct d *d) {
    WANT(wrong_first, __builtin_object_size(&a->w, 1), FLEX(128, 128, -1));
    WANT(wrong_last, __builtin_object_size(&b->w, 1), FLEX(128, -1, -1));
    WANT(wrong_scalar, __builtin_object_size(&c->w, 1), FLEX(sizeof(struct recv), -1, -1));
    WANT(wrong_member, __builtin_object_size(&d->r, 1), FLEX(sizeof(struct recv), -1, -1));
    WANT(wrong_dynamic, __builtin_dynamic_object_size(&a->w, 1), FLEX(128, 128, -1));
}
"#;

fn compile(name: &str, source: &str, level: &str) -> Output {
    compile_with(name, source, &[level])
}

fn compile_with(name: &str, source: &str, flags: &[&str]) -> Output {
    let dir = std::env::temp_dir().join(format!("rucc-member-size-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path: PathBuf = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg("--target=x86_64-unknown-linux-gnu")
        .args(flags)
        .args(["-c", "-o"])
        .arg(dir.join("one.o"))
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    out
}

#[test]
fn the_closest_member_is_answered_after_inlining() {
    for level in ["-O1", "-O2", "-Os", "-O3"] {
        let out = compile(&format!("ok{level}"), SOURCE, level);
        assert!(out.status.success(), "{level}: {}", String::from_utf8_lossy(&out.stderr));
    }
}

#[test]
fn a_fortified_copy_past_the_member_is_refused() {
    let out = compile("overflow", OVERFLOW, "-O2");
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "the overflow went through");
    assert!(said.contains("detected write beyond size of object"), "{said}");
}

#[test]
fn a_trailing_array_is_as_long_as_strict_flex_arrays_says() {
    for level in ["-O1", "-O2", "-Os"] {
        let strict = ["-DSTRICT=1", "-fstrict-flex-arrays=3", level];
        let out = compile_with(&format!("strict{level}"), TRAILING, &strict);
        assert!(out.status.success(), "strict {level}: {}", String::from_utf8_lossy(&out.stderr));
        let out = compile_with(&format!("open{level}"), TRAILING, &["-DSTRICT=0", level]);
        assert!(out.status.success(), "open {level}: {}", String::from_utf8_lossy(&out.stderr));
    }
}

#[test]
fn what_is_left_of_a_member_at_a_bounded_index_is_at_least_one_element() {
    for level in ["-O1", "-O2", "-Os", "-O3"] {
        let out = compile(&format!("bounded{level}"), BOUNDED, level);
        assert!(out.status.success(), "{level}: {}", String::from_utf8_lossy(&out.stderr));
    }
}

#[test]
fn an_array_in_a_union_inside_a_structure_is_as_long_as_its_type() {
    for level in ["-O1", "-O2", "-Os", "-O3"] {
        let out = compile(&format!("union{level}"), UNION, level);
        assert!(out.status.success(), "{level}: {}", String::from_utf8_lossy(&out.stderr));
    }
}

#[test]
fn a_member_through_a_pointer_has_no_more_than_the_whole_object_has_left() {
    for level in ["-O1", "-O2", "-Os", "-O3"] {
        let flags = ["-fstrict-flex-arrays=3", level];
        let out = compile_with(&format!("capped{level}"), CAPPED, &flags);
        assert!(out.status.success(), "{level}: {}", String::from_utf8_lossy(&out.stderr));
    }
}

#[test]
fn a_pointer_stored_just_before_it_is_read_back_is_measured() {
    for level in ["-O1", "-O2", "-Os", "-O3"] {
        let out = compile(&format!("stored{level}"), STORED, level);
        assert!(out.status.success(), "{level}: {}", String::from_utf8_lossy(&out.stderr));
    }
}

#[test]
fn a_pointer_in_a_member_of_a_local_is_measured_past_the_store_beside_it() {
    for level in ["-O1", "-O2", "-Os", "-O3"] {
        let out = compile(&format!("local{level}"), IN_LOCAL, level);
        assert!(out.status.success(), "{level}: {}", String::from_utf8_lossy(&out.stderr));
    }
}

#[test]
fn a_record_ending_in_a_flexible_array_is_the_one_the_claimed_gcc_release_says() {
    for gnuc in ["13.3.0", "14.2.0", "15.2.0", "16.2.0"] {
        let version = format!("-fgnuc-version={gnuc}");
        let flags = ["-fstrict-flex-arrays=3", version.as_str(), "-O2"];
        let out = compile_with(&format!("release{gnuc}"), RELEASE, &flags);
        assert!(out.status.success(), "{gnuc}: {}", String::from_utf8_lossy(&out.stderr));
    }
}
