//! `__seg_gs` and `__seg_fs`, end to end.
//!
//! Design: tamnd/rucc-kernel#6, the named address space part of the K5 attributes item.
//!
//! The kernel reaches its per-CPU data through `%gs` on x86-64, and from 6.9 it does that by
//! declaring the data in `__seg_gs` when the compiler passes the `CC_HAS_NAMED_AS` probe, rather
//! than by writing `%%gs:` into every template. What gcc does with them is what is checked here:
//! each access goes through the segment, the pointer conversions that would leave the space are
//! errors, and the words are keywords only in a GNU dialect on x86.

use std::path::PathBuf;
use std::process::Command;

/// The fixture, under a directory of its own so that two of these running at once do not write the
/// same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-seg-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// Whether the compiler took the source, what it wrote and what it said.
fn run(what: &str, flags: &[&str], source: &str) -> (bool, String, String) {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["-S", "-o", "-"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    let text = |bytes: Vec<u8>| String::from_utf8_lossy(&bytes).into_owned();
    (out.status.success(), text(out.stdout), text(out.stderr))
}

/// The listing for x86-64 at `-O2`, for a source that has to compile.
fn listing(what: &str, source: &str) -> String {
    let (ok, out, err) = run(what, &["--target=x86_64-unknown-linux-gnu", "-O2"], source);
    assert!(ok, "the compiler refused the fixture:\n{err}");
    out
}

/// What the compiler said about a source it has to refuse.
fn refused(what: &str, source: &str) -> String {
    let (ok, _, err) = run(what, &["--target=x86_64-unknown-linux-gnu", "-c"], source);
    assert!(!ok, "the compiler took a source it should have refused");
    err
}

#[test]
fn every_access_goes_through_the_segment() {
    let out = listing(
        "access",
        "int __seg_gs gs;\n\
         struct hot { long a, b; };\n\
         extern struct hot __seg_gs chot;\n\
         long rd(void) { return gs; }\n\
         void wr(int v) { gs = v; }\n\
         long cur(void) { return chot.b; }\n\
         long via(long __seg_gs *p) { return *p; }\n\
         void add(long __seg_gs *p, long v) { *p += v; }\n",
    );
    for want in [
        "movslq\t%gs:gs(%rip), %rax",
        "movl\t%edi, %gs:gs(%rip)",
        "movq\t%gs:chot+8(%rip), %rax",
        "movq\t%gs:(%rdi), %rax",
        "addq\t%rsi, %gs:(%rdi)",
    ] {
        assert!(out.contains(want), "no `{want}` in:\n{out}");
    }
}

/// Two reads through the same pointer are two reads, because what `%gs` points at is this CPU's
/// and the pointer is not the whole of the address.
#[test]
fn a_read_through_a_segment_is_not_merged_with_another() {
    let out = listing("twice", "long twice(long __seg_gs *p) { return *p + *p; }\n");
    assert_eq!(out.matches("%gs:(%rdi)").count(), 2, "{out}");
}

/// A memory operand of an `asm` is spelled with its segment, which is what the kernel's percpu
/// templates count on once they stop writing `%%gs:` themselves.
#[test]
fn an_asm_memory_operand_carries_its_segment() {
    let out = listing(
        "asm",
        "int __seg_gs gs;\n\
         void w(long __seg_gs *p) { asm(\"incq %0\" : \"+m\"(*p)); }\n\
         int r(void) { int v; asm(\"movl %1, %0\" : \"=r\"(v) : \"m\"(gs)); return v; }\n",
    );
    assert!(out.contains("incq\t%gs:(%rdi)"), "{out}");
    assert!(out.contains("%gs:gs(%rip)"), "{out}");
}

/// The object keeps its space and its address does not: `&gs` is the offset, and what it points
/// at is still in `__seg_gs`.
#[test]
fn the_address_of_an_object_in_a_segment_is_its_offset() {
    let out = listing(
        "address",
        "int __seg_gs gs;\n\
         int __seg_gs *tp(void) { return &gs; }\n\
         int t(void) { return _Generic(gs, int: 1, default: 0); }\n\
         unsigned long sz(void) { return sizeof(int __seg_gs); }\n",
    );
    assert!(out.contains("leaq\tgs(%rip), %rax"), "{out}");
    assert!(!out.contains("%gs:gs"), "{out}");
}

/// The shape of the kernel's probe, which is what turns the feature on.
#[test]
fn the_kernel_probe_compiles() {
    listing("probe", "int __seg_fs fs;\nint __seg_gs gs;\n");
}

#[test]
fn leaving_the_space_is_an_error() {
    let err = refused(
        "leave",
        "int __seg_gs *p;\n\
         int *leave(void) { return p; }\n",
    );
    assert!(err.contains("return from pointer to non-enclosed address space"), "{err}");

    let err = refused("both", "int __seg_gs __seg_fs x;\n");
    assert!(
        err.contains("incompatible address space qualifiers '__seg_fs' and '__seg_gs'"),
        "{err}"
    );

    let err = refused(
        "compare",
        "int __seg_gs *p;\nint __seg_fs *q;\nint same(void) { return p == q; }\n",
    );
    assert!(err.contains("comparison of pointers to disjoint address spaces"), "{err}");
}

/// An object in a segment has to be one the linker places, so the automatic ones are refused, and
/// so are a parameter and a member, which are parts of something else.
#[test]
fn only_a_static_object_is_in_a_segment() {
    let err = refused("auto", "int f(void) { int __seg_gs x = 0; return x; }\n");
    assert!(err.contains("'__seg_gs' specified for auto variable 'x'"), "{err}");

    let err = refused("param", "int f(int __seg_gs x) { return 0; }\n");
    assert!(err.contains("'__seg_gs' specified for parameter 'x'"), "{err}");

    let err = refused("member", "struct s { int __seg_fs m; };\n");
    assert!(err.contains("'__seg_fs' specified for structure field 'm'"), "{err}");

    listing(
        "static",
        "int f(void) { static int __seg_gs x; extern int __seg_gs y; return x + y; }\n",
    );
}

/// A strict dialect leaves the name to the program, and so does a machine without the segment.
#[test]
fn the_words_are_keywords_only_in_gnu_c_on_x86() {
    let source = "int __seg_gs;\n";
    let (ok, _, err) = run("c11", &["--target=x86_64-unknown-linux-gnu", "-std=c11"], source);
    assert!(ok, "{err}");
    let (ok, _, err) = run("a64", &["--target=aarch64-unknown-linux-gnu"], source);
    assert!(ok, "{err}");
}
