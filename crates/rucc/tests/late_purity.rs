//! A call to a function that only comes out as doing nothing once its own body has been through
//! the passes goes too. The kernel's `fsnotify_access` without `CONFIG_FSNOTIFY` is a chain of
//! stubs that reads the inode and then a counter behind a pointer that is always null, so the
//! purity worked out before the passes called it opaque, and at `-Os` the inliner weighs every
//! call as cold and left `vfs_read` calling a lone `ret` on every read. gcc calls nothing there.

use std::process::Command;

const SOURCE: &str = "\
struct sb_info { long watched; };
struct sb { struct sb_info *info; };
struct inode { struct sb *i_sb; unsigned int mode; };
struct dentry { struct inode *d_inode; unsigned int flags; };
struct path { void *mnt; struct dentry *dentry; };
struct file { unsigned int f_mode; struct path f_path; };
extern int __fsnotify_parent(struct dentry *, unsigned int);
static inline struct sb_info *sb_info(struct sb *sb) { return 0; }
static inline int has_watchers(struct sb *sb)
{
    struct sb_info *info = sb_info(sb);
    if (!info)
        return 0;
    return *(volatile long *)&info->watched != 0;
}
static inline int notify_parent(struct dentry *d, unsigned int mask)
{
    struct inode *inode = d->d_inode;
    if (!has_watchers(inode->i_sb))
        return 0;
    if (d->flags & 4)
        return 0;
    return __fsnotify_parent(d, mask);
}
static inline int notify_file(struct file *file, unsigned int mask)
{
    if (file->f_mode & 0x4000000)
        return 0;
    return notify_parent(file->f_path.dentry, mask);
}
static inline void notify_access(struct file *file) { notify_file(file, 1); }
extern long work(struct file *, long);
#define USE(n) long r##n(struct file *f, long x) \\
    { long v = work(f, x); if (v > 0) notify_access(f); return v; }
USE(1) USE(2) USE(3) USE(4) USE(5) USE(6) USE(7) USE(8) USE(9) USE(10) USE(11) USE(12)
";

fn compile(level: &str) -> String {
    let dir = std::env::temp_dir().join(format!("rucc-late-purity-{level}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", level, "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("assembly is text")
}

#[test]
fn a_call_to_a_body_the_passes_emptied_goes() {
    for level in ["-O2", "-Os"] {
        let text = compile(level);
        assert!(text.lines().any(|line| line == "r1:"), "{level}\n{text}");
        assert!(!text.contains("notify"), "{level}\n{text}");
    }
}
