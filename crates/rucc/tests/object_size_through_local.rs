//! What `__builtin_object_size` says about a member reached through a local pointer.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! The kernel's nouveau sizes a request it builds on the stack with
//! `__member_size(args->data) / sizeof(*args->data)`, where `args` points into a union with a byte
//! buffer and `data` is a flexible array. gcc answers that at `-O2` from the buffer `args` was set
//! to. This compiler answered it as not known, and the not known answer divided down did not fit
//! the `u8` it went into, which is a warning and an error under `CONFIG_WERROR`. The address is
//! now left for the IR, which follows `args` to the buffer, and a not known answer with arithmetic
//! on top is not warned about, since gcc has no number there while it checks the program.
//!
//! Every answer below is what gcc 16 gives for the same source once it optimizes. Each one is
//! checked by a call to a function declared with `error`, under a test the right answer makes
//! false, so a wrong answer is a refused build that names it. At `-O0` the answer waits for run
//! time and the calls stay, as they do with gcc.

use std::path::PathBuf;
use std::process::{Command, Output};

const SOURCE: &str = r#"
typedef unsigned long size_t;
struct fl { int n; char data[]; };
union un { int a; char b[4]; };
struct hasu { int n; union un u; };
extern void use(void *);
#define WANT(name, got, want) \
    do { \
        extern void name(void) __attribute__((error(#name))); \
        if ((got) != (size_t)(want)) name(); \
    } while (0)
void f(void) {
    char buf[40];
    struct fl *p = (struct fl *)buf;
    union un ubuf[4];
    union un *u = ubuf;
    struct hasu hbuf[2];
    struct hasu *h = hbuf;
    use(p);
    use(u);
    use(h);
    WANT(wrong_flex, __builtin_dynamic_object_size(p->data, 1), 36);
    WANT(wrong_flex_index, __builtin_object_size(&p->data[3], 1), 33);
    WANT(wrong_flex_whole, __builtin_object_size(p->data, 0), 36);
    WANT(wrong_flex_least, __builtin_object_size(p->data, 2), 36);
    WANT(wrong_union, __builtin_dynamic_object_size(u->b, 1), 16);
    WANT(wrong_member, __builtin_dynamic_object_size(&h->u, 1), 4);
}
"#;

/// nouveau's `nouveau_channels_init`, cut down to the line that was refused, and the same line
/// over a parameter, whose answer is not known at any level.
const NOUVEAU: &str = r#"
struct nv_device_info_v1_data { unsigned long mthd; unsigned long data; };
struct nv_device_info_v1 {
    unsigned char version;
    unsigned char count;
    unsigned char pad02[6];
    struct nv_device_info_v1_data data[];
};
extern int use(void *);
int f(void) {
    union {
        unsigned char bytes[sizeof(struct nv_device_info_v1)
                            + 2 * sizeof(struct nv_device_info_v1_data)];
        struct nv_device_info_v1 obj;
    } buf = {};
    struct nv_device_info_v1 *args = &buf.obj;
    args->version = 1;
    args->count = __builtin_dynamic_object_size(args->data, 1) / sizeof(*args->data);
    return use(args);
}
void g(struct nv_device_info_v1 *args) {
    args->count = __builtin_dynamic_object_size(args->data, 1) / sizeof(*args->data);
}
"#;

fn compile(name: &str, source: &str, level: &str) -> Output {
    let dir = std::env::temp_dir().join(format!("rucc-local-size-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path: PathBuf = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=aarch64-unknown-linux-gnu", "-fstrict-flex-arrays=3", "-Werror", level])
        .args(["-c", "-o"])
        .arg(dir.join("one.o"))
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    out
}

#[test]
fn a_member_through_a_local_pointer_is_measured_from_what_it_was_set_to() {
    for level in ["-O1", "-O2", "-Os", "-O3"] {
        let out = compile(&format!("ok{level}"), SOURCE, level);
        assert!(out.status.success(), "{level}: {}", String::from_utf8_lossy(&out.stderr));
    }
}

#[test]
fn a_size_not_known_yet_is_not_warned_about_when_divided_down() {
    for level in ["-O0", "-O2"] {
        let out = compile(&format!("nouveau{level}"), NOUVEAU, level);
        assert!(out.status.success(), "{level}: {}", String::from_utf8_lossy(&out.stderr));
    }
}
