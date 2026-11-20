//! A null pointer passed to a static function counts as a constant when the inliner weighs the
//! copy, as an integer constant does, so the arms that test it are not counted.
//!
//! `init_vma_prep` in mm/vma.c passes `NULL` for the `vmg` of `init_multi_vma_prep`, and most of
//! that body tests `vmg`. A null pointer is an `inttoptr` of a zero, which the weighing did not
//! see through, so the body was counted whole, too large, and stayed a call in `__split_vma`,
//! `vma_shrink` and `commit_merge`, where gcc inlines it.

use std::process::Command;

const SOURCE: &str = "\
struct file { void *f_mapping; };
struct vma { char pad[64]; void *anon_vma; char p2[16]; struct file *vm_file; };
struct vmg {
\tchar pad[24];
\tstruct vma *middle, *next;
\tchar p3[88];
\tunsigned remove_middle:1, adjust_next:1, adjust_middle:1, skip:1, remove_next:1;
};
struct vp {
\tstruct vma *vma, *adj_next;
\tstruct file *file;
\tvoid *mapping, *anon_vma;
\tstruct vma *remove, *remove2;
\tvoid *insert;
\t_Bool skip;
};
static void prep(struct vp *vp, struct vma *vma, struct vmg *vmg)
{
\tstruct vma *adjust;
\tstruct vma **remove = &vp->remove;
\t__builtin_memset(vp, 0, sizeof(struct vp));
\tvp->vma = vma;
\tvp->anon_vma = vma->anon_vma;
\tif (vmg && vmg->remove_middle) {
\t\t*remove = vmg->middle;
\t\tremove = &vp->remove2;
\t}
\tif (vmg && vmg->remove_next)
\t\t*remove = vmg->next;
\tif (vmg && vmg->adjust_middle)
\t\tadjust = vmg->middle;
\telse if (vmg && vmg->adjust_next)
\t\tadjust = vmg->next;
\telse
\t\tadjust = 0;
\tvp->adj_next = adjust;
\tif (!vp->anon_vma && adjust)
\t\tvp->anon_vma = adjust->anon_vma;
\tvp->file = vma->vm_file;
\tif (vp->file)
\t\tvp->mapping = vma->vm_file->f_mapping;
\tif (vmg && vmg->skip)
\t\tvp->skip = 1;
}
void use(struct vp *);
void a(struct vma *v) { struct vp vp; prep(&vp, v, 0); use(&vp); }
void b(struct vma *v) { struct vp vp; prep(&vp, v, 0); use(&vp); }
void c(struct vma *v, struct vmg *g) { struct vp vp; prep(&vp, v, g); use(&vp); }
";

/// The IR the compiler produced for the source at `-O2`.
fn ir(target: &str) -> String {
    let dir = std::env::temp_dir()
        .join(format!("rucc-inline-null-argument-{}-{target}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
        .args(["-O2", "--emit=ir", "-w", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_body_passed_null_is_weighed_without_the_arms_that_test_it() {
    for target in ["i686-linux-gnu", "x86_64-linux-gnu"] {
        let text = ir(target);
        let left = text.lines().filter(|line| calls(line) && line.contains(" @prep(")).count();
        assert_eq!(left, 0, "{target}\n{text}");
    }
}

/// Whether a line of IR is a call, whatever attributes it has, as in `call.nofree`.
fn calls(line: &str) -> bool {
    line.split_whitespace().any(|word| word == "call" || word.starts_with("call."))
}
