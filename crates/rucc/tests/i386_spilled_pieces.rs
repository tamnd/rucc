//! A value that goes to the stack on i386 because a loop further down has more values than there
//! are registers, and is then read over and over in a block that has registers to spare, is read
//! off the stack once in that block. The allocator places a value for its whole life, so `dev`
//! below used to be loaded from its slot before each of the seven reads in the first block, the
//! way `super_90_sync` in drivers/md/md.c loaded the device and the superblock pointer before every
//! store. See `rucc_regalloc::pieces`.

use std::collections::HashMap;
use std::process::Command;

const SOURCE: &str = "\
struct sb { unsigned a, b, c, d, e, f, g, h; };
struct dev { struct sb *sb; unsigned x, y, z, w; };
unsigned fill(struct dev *dev, const unsigned *p, unsigned n)
{
    struct sb *sb = dev->sb;
    unsigned s0 = 0, s1 = 1, s2 = 2, s3 = 3;
    sb->a = dev->x;
    sb->b = dev->y;
    sb->c = dev->z;
    sb->d = dev->w;
    sb->e = dev->x ^ dev->y;
    sb->f = dev->z ^ dev->w;
    for (unsigned i = 0; i < n; i++) {
        s0 += p[i] ^ s3;
        s1 += s0 ^ i;
        s2 ^= s1 + p[i + 1];
        s3 += s2 ^ n;
    }
    sb->g = s0 ^ s1;
    sb->h = s2 ^ s3;
    return dev->x + sb->a;
}
";

fn listing(level: &str) -> String {
    let dir = std::env::temp_dir()
        .join(format!("rucc-i386-spilled-pieces-{}-{level}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=i686-unknown-linux-gnu", level, "-fno-pic"])
        .args(["-fno-asynchronous-unwind-tables", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("a listing is text")
}

#[test]
fn a_spilled_pointer_is_loaded_once_in_the_first_block() {
    for level in ["-O2", "-Os"] {
        let listing = listing(level);
        let start = listing.find("\nfill:\n").expect("fill");
        let mut loads: HashMap<&str, usize> = HashMap::new();
        for line in listing[start..].lines().skip(2).take_while(|line| !line.starts_with("\tj")) {
            let Some(rest) = line.strip_prefix("\tmovl\t") else { continue };
            let Some((from, into)) = rest.split_once(", ") else { continue };
            if from.ends_with("(%esp)") && into.starts_with("%e") {
                *loads.entry(from).or_default() += 1;
            }
        }
        let most = loads.values().copied().max().unwrap_or(0);
        assert!(most <= 2, "{level}: {loads:?}\n{listing}");
    }
}
