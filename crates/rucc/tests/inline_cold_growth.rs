//! A call that is not hot, which at `-Os` is every call, goes into its caller only when the
//! program does not grow by it. That is gcc's `growth_positive_p` in
//! `want_inline_small_function_p`: the copies at every call against the body out of line that goes
//! away once the last call is gone.
//!
//! In the kernel `signal_pending` asks `test_ti_thread_flag` about two flags it knows, and with the
//! flag known the body is a load and a test. gcc copies both and drops the function, where rucc
//! left both calls and 45 copies of it out of line in a tinyconfig kernel, two of them on every
//! `read` from /dev/zero.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, so the listing is the same on
/// every machine that runs the suite.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The assembly the compiler writes for that source at `-Os`.
fn asm(what: &str, source: &str) -> String {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("rucc-cold-growth-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(["-Os", "-fno-pie", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "the compiler refused the fixture:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("what the compiler writes is text")
}

/// How many calls to `name` the listing makes.
fn calls(listing: &str, name: &str) -> usize {
    listing.lines().filter(|line| line.trim() == format!("call\t{name}")).count()
}

/// `test_ti_thread_flag` and the bit test under it as the kernel has them on x86-64.
const FLAGS: &str = r#"
#define __always_inline inline __attribute__((__always_inline__))
struct thread_info { unsigned long flags; unsigned long syscall_work; unsigned int status; };
struct task_struct { struct thread_info thread_info; unsigned int jobctl; };
static __always_inline int constant_test_bit(long nr, const volatile unsigned long *addr)
{
	return ((1UL << (nr & 63)) & (addr[nr >> 6])) != 0;
}
static __always_inline _Bool variable_test_bit(long nr, volatile const unsigned long *addr)
{
	_Bool oldbit;
	asm volatile("btq %2,%1" : "=@ccc" (oldbit)
		     : "m" (*(unsigned long *)addr), "Ir" (nr) : "memory");
	return oldbit;
}
static __always_inline _Bool test_bit(unsigned long nr, const volatile unsigned long *addr)
{
	return __builtin_constant_p(nr) ? constant_test_bit(nr, addr) : variable_test_bit(nr, addr);
}
static inline int test_ti_thread_flag(struct thread_info *ti, int flag)
{
	return test_bit(flag, (unsigned long *)&ti->flags);
}
static inline int signal_pending(struct task_struct *p)
{
	if (__builtin_expect(test_ti_thread_flag(&p->thread_info, 10), 0))
		return 1;
	return __builtin_expect(test_ti_thread_flag(&p->thread_info, 2), 0);
}
extern struct task_struct *cur(void);
extern void work(void);
long wait(long n)
{
	for (long i = 0; i < n; i++) {
		if (signal_pending(cur()))
			return -4;
		work();
	}
	return 0;
}
"#;

#[test]
fn a_known_flag_is_tested_in_place() {
    let listing = asm("flags", FLAGS);
    assert_eq!(calls(&listing, "test_ti_thread_flag"), 0, "{listing}");
    assert!(!listing.contains("test_ti_thread_flag:"), "{listing}");
    assert!(listing.contains("$1024"), "flag 10 is a test of its bit:\n{listing}");
}

/// A loop copied at three calls is more than the one body out of line, so gcc leaves all three
/// calls and so does this.
const MIX: &str = r#"
extern void use(int);
static int mix(const int *p, int n)
{
	int s = 0;
	for (int i = 0; i < n; i++)
		s = s * 31 + p[i] * (i | 1);
	return s ^ (s >> 7);
}
void a(const int *p, int n) { use(mix(p, n)); }
void b(const int *p, int n) { use(mix(p + 1, n)); }
void c(const int *p, int n) { use(mix(p + 2, n)); }
"#;

#[test]
fn a_body_that_grows_the_program_stays_a_call() {
    let listing = asm("mix", MIX);
    assert_eq!(calls(&listing, "mix"), 3, "{listing}");
}
