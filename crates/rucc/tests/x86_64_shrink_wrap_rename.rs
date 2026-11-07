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
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
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

/// The same with the kernel's entity and run queue, which is how `__update_load_avg_se` calls it.
/// `cfs_rq->curr == se` is a comparison whose byte only the branch behind it reads, and the layout
/// writes the two as a `cmpq` and a `jne`. The allocator gave that byte `%r11`, the one register a
/// call clobbers that held nothing there, and the rename took the byte as written and so found no
/// room for `%rbx` and `%rbp`.
const JOINED: &str = "\
struct se {
    char pad[0x50];
    unsigned long weight;
    char on_rq;
    char p3[0xb8 - 0x59];
    void *my_q;
    unsigned long runnable_weight;
    char p5[0x100 - 0xc8];
    struct sa avg;
};
struct cfs { char pad[0x50]; struct se *curr; };
static unsigned long run(struct se *se) { return se->my_q ? se->runnable_weight : !!se->on_rq; }
int upd_se(u64 now, struct cfs *cfs_rq, struct se *se) {
    if (sum(now, &se->avg, !!se->on_rq, run(se), cfs_rq->curr == se)) {
        struct sa *sa = &se->avg;
        u32 d = 47742 + sa->period_contrib;
        sa->load_avg = se->weight * sa->load_sum / d;
        sa->runnable_avg = sa->runnable_sum / d;
        sa->util_avg = sa->util_sum / d;
        return 1;
    }
    return 0;
}
";

/// A run of [`JOINED`] over two entities, one of them the current one, that prints what it leaves
/// behind.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
const JOINED_MAIN: &str = "\
#include <stdio.h>
int main(void) {
    static struct se ses[2];
    struct cfs cfs = { { 0 }, &ses[0] };
    unsigned long long now = 4000;
    int ups = 0;
    for (int i = 0; i < 4000; i++) {
        struct se *se = &ses[i % 2];
        se->weight = 1024 + i % 5;
        se->on_rq = (char)(i % 3 != 0);
        se->my_q = i % 4 == 0 ? se : 0;
        se->runnable_weight = 3 + i % 7;
        now += (unsigned long long)(i * 7919 % 300000) - (i % 11 == 0 ? 900000 : 0);
        ups += upd_se(now, &cfs, se);
        if (i % 97 == 0)
            printf(\"%llu %llu %llu %u %u %lu %lu %lu\\n\", se->avg.last, se->avg.load_sum,
                   se->avg.runnable_sum, se->avg.util_sum, se->avg.period_contrib,
                   se->avg.load_avg, se->avg.runnable_avg, se->avg.util_avg);
    }
    printf(\"%d\\n\", ups);
    return 0;
}
";

/// The shape of `rcu_preempt_deferred_qs_irqrestore` in kernel/rcu/tree_plugin.h. The task
/// pointer is copied in the entry and first read after a join, and the function spills further in,
/// so the allocator holds `%r10` and `%r11` back as scratch. The rename put the task pointer in
/// `%r10`, and the pass that forwards a copy into a scratch register to its readers in the block
/// found none in the entry and took the copy out. The kernel built that way did not boot.
const KEPT: &str = "\
struct task { char pad[1152]; unsigned int special; };
struct cpu { int pending; char p[13]; char exp; };
extern struct cpu cpus[];
void qs(void);
void report(struct cpu *);
void unblock(struct task *);
long rq(struct task *t, unsigned long flags, int which) {
    struct cpu *c = &cpus[which];
    if (c->pending == 1)
        c->pending = 0;
    unsigned s = t->special;
    if (!s && !c->exp)
        return flags & 0x200;
    t->special = 0;
    if (s & 0xff00)
        qs();
    if (c->exp)
        report(c);
    if (s & 0xff)
        unblock(t);
    if (c->exp)
        report(c);
    c->pending += 2;
    if (c->p[0])
        report(c);
    c->p[1] = 0;
    long v0 = c->p[2], v1 = c->p[3], v2 = c->p[4], v3 = c->p[5], v4 = c->p[6];
    long v5 = c->p[7], v6 = c->p[8], v7 = c->p[9];
    qs();
    c->p[10] = v0 * v1 + v2 * v3 + v4 * v5 + v6 * v7;
    return flags & 0x200;
}
";

/// What [`KEPT`] calls, and a run over a few tasks that prints what it leaves behind.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
const KEPT_MAIN: &str = "\
#include <stdio.h>
struct cpu cpus[4];
static int calls;
void qs(void) { calls += 1; }
void report(struct cpu *c) { calls += 10; c->exp = 0; }
void unblock(struct task *t) { calls += 100; t->pad[3] = 1; }
static struct task tasks[3];
int main(void) {
    long sum = 0;
    for (int i = 0; i < 300; i++) {
        struct task *t = &tasks[i % 3];
        t->special = (unsigned)(i * 37) & 0x10101;
        cpus[i % 4].pending = i % 2;
        cpus[i % 4].exp = (char)(i % 5 == 0);
        cpus[i % 4].p[0] = (char)(i % 7 == 0);
        sum += rq(t, (unsigned long)i * 0x80, i % 4);
    }
    printf(\"%ld %d %d %d\\n\", sum, calls, cpus[1].p[10], tasks[2].pad[3]);
    return 0;
}
";

/// The shape of `getresgid` in kernel/sys.c, with the kernel's `put_user`. Its template is kept as
/// text because of the `%c`, and it says `%ebx` is clobbered, since `__put_user_4` uses it. The
/// rename took that for a register it could swap in the first two, and the prologue went in front
/// of the third, so the first two wrote over the caller's `%rbx`.
const PUT: &str = "\
register unsigned long current_stack_pointer asm(\"rsp\");
#define put(x, ptr) ({ \\
    int ret; void *at = (ptr); register int val asm(\"%eax\") = (x); \\
    asm volatile(\"call __put_user_%c[size]\" : \"=c\"(ret), \"+r\"(current_stack_pointer) \\
                 : \"0\"(at), \"r\"(val), [size] \"i\"(4) : \"ebx\"); \\
    __builtin_expect(ret, 0); })
struct regs { long pad[12]; long dx, si, di; };
struct cred { int a, b, c, gid, d, sgid, e, egid; };
extern int overflowgid;
extern struct cred *cur;
static int munge(int v) { return v == -1 ? overflowgid : v; }
long res(struct regs *regs) {
    struct cred *cred = cur;
    int x = munge(cred->gid), y = munge(cred->egid), z = munge(cred->sgid);
    int r = put(x, (int *)regs->di);
    if (!r) {
        r = put(y, (int *)regs->si);
        if (!r)
            r = put(z, (int *)regs->dx);
    }
    return r;
}
";

/// `_parse_integer_limit` in lib/kstrtox.c of Linux 7.3, which takes the first value of the sum
/// as an argument. The rename moved the digit out of `%rbx` into `%r10`, after which nothing wanted
/// the frame and the prologue stayed at the top, and the copy of a hex digit into `%r10` was taken
/// out as a copy into a scratch register nothing read. `ffff` came out as 0, and the kernel failed
/// its scanf and cmdline tests.
const PARSE: &str = "\
#include <limits.h>
#include <stddef.h>
__attribute__((noinline)) unsigned int parse(const char *s, unsigned int base,
                                             unsigned long long *p, size_t max,
                                             unsigned long long init) {
    unsigned int rv, overflow = 0;
    unsigned long long res = init;
    for (rv = 0; rv < max; rv++, s++) {
        unsigned int c = *s;
        unsigned int lc = (unsigned char)c | 0x20;
        unsigned int val;
        if ('0' <= c && c <= '9')
            val = c - '0';
        else if ('a' <= lc && lc <= 'f')
            val = lc - 'a' + 10;
        else
            break;
        if (val >= base)
            break;
        if (__builtin_expect(!!(res & (~0ull << 60)), 0)) {
            if (__builtin_mul_overflow(res, base, &res) ||
                __builtin_add_overflow(res, val, &res)) {
                res = ULLONG_MAX;
                overflow = 1u << 31;
            }
        } else {
            res = res * base + val;
        }
    }
    *p = res;
    return rv | overflow;
}
";

/// A run of [`PARSE`] over decimal and hex strings that prints what it read.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
const PARSE_MAIN: &str = "\
#include <stdio.h>
int main(void) {
    const char *s[] = { \"123\", \"ffff\", \"7fffffff\", \"DeadBeef\", \"18446744073709551615\",
                        \"99999999999999999999\", \"12x\", \"\" };
    unsigned b[] = { 10, 16, 16, 16, 10, 10, 10, 10 };
    for (int i = 0; i < 8; i++) {
        unsigned long long r;
        unsigned n = parse(s[i], b[i], &r, INT_MAX, 0);
        printf(\"%u %llu\\n\", n, r);
    }
    unsigned long long r;
    unsigned n = parse(\"a5\", 16, &r, INT_MAX, 7);
    printf(\"%u %llu\\n\", n, r);
    n = parse(\"abcdef\", 16, &r, 3, 0);
    printf(\"%u %llu\\n\", n, r);
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
    listing_of(SOURCE)
}

fn listing_of(source: &str) -> String {
    let dir = scratch();
    let path = dir.join("pelt.c");
    std::fs::write(&path, source).expect("the fixture can be written");
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

/// Whether some `ret` in the body comes right after something other than a `pop`, which is a way
/// out that never took the frame. The early returns are guessed not taken, as gcc guesses them, so
/// they are laid out after the rest of the function rather than in front of the prologue.
fn returns_bare(body: &[String]) -> bool {
    let code: Vec<&String> = body.iter().filter(|line| !line.starts_with('.')).collect();
    code.windows(2).any(|pair| pair[1] == "ret" && !pair[0].starts_with("pop"))
}

#[test]
fn the_early_returns_push_nothing() {
    let text = listing();
    let body = lines(&text, "upd");
    let first = body.iter().position(|line| line.starts_with("push")).expect("a prologue");
    let branches = body[..first].iter().filter(|line| line.starts_with('j')).count();
    assert!(returns_bare(&body) && branches >= 2, "{text}");
}

#[test]
fn a_joined_comparison_leaves_its_byte_free() {
    let text = listing_of(&format!("{SOURCE}{JOINED}"));
    let body = lines(&text, "upd_se");
    let first = body.iter().position(|line| line.starts_with("push")).expect("a prologue");
    let branches = body[..first].iter().filter(|line| line.starts_with('j')).count();
    assert!(returns_bare(&body) && branches >= 2, "{text}");
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn the_joined_comparison_works_out_what_it_did_before() {
    let dir = scratch();
    let source = dir.join("joined.c");
    let text = format!("{SOURCE}{JOINED}{JOINED_MAIN}");
    std::fs::write(&source, text).expect("the fixture can be written");
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

/// Each of the two scratch registers is written before it is read on the way to the prologue,
/// which it was not when the copy of the task pointer into `%r10` was taken out.
#[test]
fn a_value_moved_into_a_scratch_register_is_put_there() {
    let text = listing_of(KEPT);
    let body = lines(&text, "rq");
    let first = body.iter().position(|line| line.starts_with("push")).expect("a prologue");
    for reg in ["%r10", "%r11"] {
        let Some(line) = body[..first].iter().find(|line| line.contains(reg)) else { continue };
        let (_, operands) = line.split_once(char::is_whitespace).expect("an instruction");
        let (read, written) = operands.rsplit_once(',').unwrap_or(("", operands));
        assert!(!read.contains(reg) && written.trim().starts_with(reg), "{text}");
    }
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn the_kept_value_works_out_what_it_did_before() {
    let dir = scratch();
    let source = dir.join("kept.c");
    std::fs::write(&source, format!("{KEPT}{KEPT_MAIN}")).expect("the fixture can be written");
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

/// `%rbx` is saved before the first template that clobbers it runs.
#[test]
fn a_register_a_template_clobbers_is_saved_before_it() {
    let text = listing_of(PUT);
    let body = lines(&text, "res");
    let push = body.iter().position(|line| line.starts_with("pushq\t%rbx")).expect("a prologue");
    let call = body.iter().position(|line| line.starts_with("call")).expect("a call");
    assert!(push < call, "{text}");
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn a_rename_kept_without_the_wrap_keeps_its_copies() {
    let dir = scratch();
    let source = dir.join("kstrtox.c");
    std::fs::write(&source, format!("{PARSE}{PARSE_MAIN}")).expect("the fixture can be written");
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
    assert!(said[0].starts_with("3 123\n4 65535\n8 2147483647\n"), "{}", said[0]);
    assert_eq!(said[0], said[1]);
    std::fs::remove_dir_all(&dir).ok();
}
