//! The `rucc` binary.
//!
//! Deliberately empty. Everything lives in `rucc-driver` so that the whole driver, including
//! argument parsing and the exit code, is reachable from a test without spawning a process. The
//! one thing here is the allocator, which only a process can choose.

mod pool;

#[global_allocator]
static POOL: pool::Pool = pool::Pool;

fn main() -> std::process::ExitCode {
    huge_pages();
    pool::keep_freed_memory();
    let mut args = std::env::args();
    let program = args.next().unwrap_or_default();
    let args: Vec<String> = args.collect();
    std::process::ExitCode::from(u8::try_from(rucc_driver::run_as(&program, &args)).unwrap_or(1))
}

/// Runs the compiler again with glibc told to back its heap with huge pages, where the kernel
/// gives them only to a process that asks.
///
/// A compile touches its heap once and then exits, so most of its system time is the kernel
/// faulting in 4 KiB pages one at a time. On net/core/filter.c from Linux 7.3 that was 55822
/// faults, and with `glibc.malloc.hugetlb=1` it is 1835, and the compile takes about a fifth less
/// CPU time. glibc reads its tunables only when a process starts, so the one way to set it is to
/// run again with it in the environment. That costs one exec, which is small next to the faults
/// it saves. Nothing is done when the variable already names the tunable, which is also what stops
/// the second run from doing it again, and nothing is done when the kernel gives huge pages to
/// every process or to none.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn huge_pages() {
    use std::os::unix::process::CommandExt;
    const TUNABLE: &str = "glibc.malloc.hugetlb";
    let current = std::env::var("GLIBC_TUNABLES").unwrap_or_default();
    if current.contains(TUNABLE) || std::env::var_os("RUCC_NO_HUGE_PAGES").is_some() {
        return;
    }
    let advised = std::fs::read_to_string("/sys/kernel/mm/transparent_hugepage/enabled")
        .is_ok_and(|mode| mode.contains("[madvise]"));
    if !advised {
        return;
    }
    let value =
        if current.is_empty() { format!("{TUNABLE}=1") } else { format!("{current}:{TUNABLE}=1") };
    let mut args = std::env::args_os();
    let Some(program) = args.next() else { return };
    // Only comes back when the exec failed, and then this run goes on without huge pages.
    let _ = std::process::Command::new("/proc/self/exe")
        .arg0(program)
        .args(args)
        .env("GLIBC_TUNABLES", value)
        .exec();
}

/// Nothing to ask for on a C library without the tunable.
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn huge_pages() {}
