//! A value the allocator put in a saved register in front of an early return is moved to one a
//! call clobbers, so the early return saves nothing.
//!
//! `__update_load_avg_se` in kernel/sched/pelt.c works out how long it has been since the last
//! update and returns at once when that is under a microsecond, which is most calls. The value was
//! in `%rbx`, so the six pushes went at the top and ran on every call. gcc has it in a register a
//! call clobbers there and pushes nothing on that path. The fixture is the function with the
//! kernel's types made small. `fork` in the kernel bench calls it several times per fork.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SOURCE: &str = "\
typedef unsigned long long u64;
typedef unsigned int u32;
struct sa {
    u64 last, load_sum, runnable_sum;
    u32 util_sum, period_contrib;
    unsigned long load_avg, runnable_avg, util_avg;
};
const u32 inv[32] = {
    0xffffffff, 0xfa83b2da, 0xf5257d14, 0xefe4b99a, 0xeac0c6e6, 0xe5b906e6, 0xe0ccdeeb, 0xdbfbb796,
    0xd744fcc9, 0xd2a81d91, 0xce248c14, 0xc9b9bd85, 0xc5672a10, 0xc12c4cc9, 0xbd08a39e, 0xb8fbaf46,
    0xb504f333, 0xb123f581, 0xad583ee9, 0xa9a15ab4, 0xa5fed6a9, 0xa2704302, 0x9ef5325f, 0x9b8d39b9,
    0x9837f050, 0x94f4efa8, 0x91c3d373, 0x8ea4398a, 0x8b95c1e3, 0x88980e80, 0x85aac367, 0x82cd8698,
};
static u64 decay(u64 v, u64 n) {
    if (__builtin_expect(n > 32 * 63, 0))
        return 0;
    u32 l = n;
    if (__builtin_expect(l >= 32, 0)) {
        v >>= l / 32;
        l %= 32;
    }
    return (u64)(((unsigned __int128)v * inv[l]) >> 32);
}
static inline __attribute__((always_inline)) int sum(u64 now, struct sa *sa, unsigned long load,
                                                     unsigned long runnable, int running) {
    u64 delta = now - sa->last;
    if ((long long)delta < 0) {
        sa->last = now;
        return 0;
    }
    delta >>= 10;
    if (!delta)
        return 0;
    sa->last += delta << 10;
    if (!load)
        runnable = running = 0;
    u32 contrib = delta;
    delta += sa->period_contrib;
    u64 periods = delta / 1024;
    if (periods) {
        sa->load_sum = decay(sa->load_sum, periods);
        sa->runnable_sum = decay(sa->runnable_sum, periods);
        sa->util_sum = decay(sa->util_sum, periods);
        delta %= 1024;
        if (load)
            contrib = 1024 - sa->period_contrib + delta + periods * 7;
    }
    sa->period_contrib = delta;
    if (load)
        sa->load_sum += load * contrib;
    if (runnable)
        sa->runnable_sum += runnable * contrib << 10;
    if (running)
        sa->util_sum += contrib << 10;
    return periods != 0;
}
int upd(u64 now, struct sa *sa, unsigned long w, int on, int cur) {
    if (sum(now, sa, !!on, on, cur)) {
        u32 d = 47742 + sa->period_contrib;
        sa->load_avg = w * sa->load_sum / d;
        sa->runnable_avg = sa->runnable_sum / d;
        sa->util_avg = sa->util_sum / d;
        return 1;
    }
    return 0;
}
";

/// Runs it over a spread of times and prints what it leaves behind, so that two builds can be
/// held up against each other.
const MAIN: &str = "\
#include <stdio.h>
int main(void) {
    struct sa sa = { 5000, 100, 200, 300, 7, 0, 0, 0 };
    unsigned long long now = 4000;
    int ups = 0;
    for (int i = 0; i < 4000; i++) {
        now += (unsigned long long)(i * 7919 % 300000) - (i % 11 == 0 ? 900000 : 0);
        ups += upd(now, &sa, 1024 + i % 5, i % 3, i % 2);
        if (i % 97 == 0)
            printf(\"%llu %llu %llu %u %u %lu %lu %lu\\n\", sa.last, sa.load_sum,
                   sa.runnable_sum, sa.util_sum, sa.period_contrib, sa.load_avg,
                   sa.runnable_avg, sa.util_avg);
    }
    printf(\"%d\\n\", ups);
    return 0;
}
";

fn scratch() -> std::path::PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-x64-rename-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir
}

fn listing() -> String {
    let dir = scratch();
    let path = dir.join("pelt.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-O2", "-fno-pic", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("a listing is text")
}

/// The lines of one function, labels and directives included.
fn lines(text: &str, name: &str) -> Vec<String> {
    text.lines()
        .skip_while(|line| *line != format!("{name}:"))
        .skip(1)
        .take_while(|line| !line.starts_with("\t.size"))
        .map(|line| line.trim().to_string())
        .collect()
}

#[test]
fn the_early_returns_push_nothing() {
    let text = listing();
    let body = lines(&text, "upd");
    let first = body.iter().position(|line| line.starts_with("push")).expect("a prologue");
    let rets = body[..first].iter().filter(|line| *line == "ret").count();
    let branches = body[..first].iter().filter(|line| line.starts_with('j')).count();
    assert!(rets >= 1 && branches >= 2, "{text}");
}

/// Read in the order of the listing, as the unwinder reads them, the distance to the return
/// address is 8 at each `ret`, which it is not when a copy of a block after the prologue was left
/// with the rows of the one before it.
#[test]
fn the_unwind_rows_follow_the_frame() {
    let text = listing();
    let mut cfa = 8;
    let mut kept = Vec::new();
    for line in lines(&text, "upd") {
        if let Some(at) = line.strip_prefix(".cfi_def_cfa_offset ") {
            cfa = at.parse().expect("a distance");
        } else if line == ".cfi_remember_state" {
            kept.push(cfa);
        } else if line == ".cfi_restore_state" {
            cfa = kept.pop().expect("a remembered state");
        } else if line == "ret" {
            assert_eq!(cfa, 8, "{text}");
        }
    }
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn it_works_out_what_it_did_before() {
    let dir = scratch();
    let source = dir.join("pelt.c");
    std::fs::write(&source, format!("{SOURCE}{MAIN}")).expect("the fixture can be written");
    let mut said = Vec::new();
    for level in ["-O0", "-O2"] {
        let program = dir.join(level);
        let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
            .arg(level)
            .arg("-o")
            .arg(&program)
            .arg(&source)
            .output()
            .expect("the compiler is built before its own tests run");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "the compiler refused the fixture at {level}:\n{err}");
        let run = Command::new(&program).output().expect("the program runs");
        assert!(run.status.success(), "the program failed at {level}");
        said.push(String::from_utf8(run.stdout).expect("the output is text"));
    }
    assert_eq!(said[0], said[1]);
    std::fs::remove_dir_all(&dir).ok();
}
