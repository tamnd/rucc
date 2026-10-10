//! A function whose epilogue puts back two registers or more returns from one block, which every
//! other return jumps to, the way gcc writes it. Every return used to have an epilogue of its own,
//! and in drivers/md/md.c on i386 that was eight copies of four pops and a `ret` in one function.
//! A return that is a call made in tail position keeps its own epilogue, since the jump to the
//! callee is written where the `ret` would be.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static RUN: AtomicUsize = AtomicUsize::new(0);

const SOURCE: &str = "\
extern int f(int);
extern int g(int, int);
int pick(int a, int b, int c)
{
    int x = f(a), y = f(b), z = f(c);
    if (x > y)
        return g(x, z) + y;
    if (y > z)
        return f(x + z) - y;
    return x + y + z;
}
int away(int a, int b)
{
    int x = f(a), y = f(b);
    if (x > y)
        return g(x, y);
    if (y > 3)
        return x * y + f(y);
    return x + y;
}
";

fn listing(target: &str, level: &str) -> String {
    let dir = std::env::temp_dir().join(format!(
        "rucc-shared-exit-{}-{}",
        std::process::id(),
        RUN.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
        .args(target.starts_with("i686").then_some("-mregparm=3"))
        .args([level, "-fno-pic", "-fno-asynchronous-unwind-tables", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("a listing is text")
}

fn body<'a>(text: &'a str, name: &str) -> &'a str {
    let start = text.find(&format!("\n{name}:\n")).expect("the function is in the listing");
    let end = text[start..].find("\t.size").map_or(text.len(), |end| start + end);
    &text[start..end]
}

#[test]
fn every_return_jumps_to_one_epilogue() {
    for target in ["i686-unknown-linux-gnu", "x86_64-unknown-linux-gnu"] {
        for level in ["-O2", "-Os"] {
            let text = listing(target, level);
            let pick = body(&text, "pick");
            assert_eq!(pick.matches("\tret").count(), 1, "{target} {level}:\n{pick}");
        }
    }
}

#[test]
fn a_call_in_tail_position_keeps_its_own_epilogue() {
    for target in ["i686-unknown-linux-gnu", "x86_64-unknown-linux-gnu"] {
        let text = listing(target, "-O2");
        let away = body(&text, "away");
        assert!(away.contains("\tjmp\tg"), "{target}:\n{away}");
        assert_eq!(away.matches("\tret").count(), 1, "{target}:\n{away}");
    }
}
