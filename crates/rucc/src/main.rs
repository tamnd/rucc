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
///
/// Nothing is done either when the kernel has few free blocks of 2 MiB. Then each huge page fault
/// stops to compact memory, and on a busy host that mostly fails. On a host with five virtual
/// machines and a kernel build running, 200 compiles from a Linux 7.3 `defconfig` took 102 to 128
/// seconds of system time with the tunable and 36 to 39 without it, and the faults were nearly
/// the same, so the compiles paid for the compaction and got few huge pages.
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
    let free = std::fs::read_to_string("/proc/buddyinfo").map_or(0, |text| free_huge_blocks(&text));
    if free < ENOUGH_HUGE_BLOCKS {
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

/// How many free blocks of 2 MiB the kernel must have before a compile asks for huge pages.
///
/// This is 1 GiB, which is enough for eight large compiles at the same time.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
const ENOUGH_HUGE_BLOCKS: u64 = 512;

/// The free blocks of 2 MiB in `/proc/buddyinfo`, over all nodes and zones.
///
/// Each line names the node and the zone, then gives the free blocks of each order from 0 up. A
/// block of order 9 is 2 MiB with 4 KiB pages, and a block of a higher order counts as as many
/// blocks of 2 MiB as it holds.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn free_huge_blocks(text: &str) -> u64 {
    text.lines()
        .filter_map(|line| line.split_once("zone"))
        .flat_map(|(_, rest)| {
            rest.split_whitespace()
                .skip(1)
                .enumerate()
                .filter(|&(order, _)| order >= 9)
                .map(|(order, count)| count.parse::<u64>().unwrap_or(0) << (order - 9).min(32))
        })
        .sum()
}

/// Nothing to ask for on a C library without the tunable.
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn huge_pages() {}

#[cfg(all(test, target_os = "linux", target_env = "gnu"))]
mod tests {
    use super::free_huge_blocks;

    #[test]
    fn the_free_blocks_of_2_mib_are_counted_over_each_zone() {
        let text = "Node 0, zone      DMA      0      0      0      0      0      0      0      0      1      1      2 \n\
                    Node 0, zone    DMA32   5956   4397   3511   6727   4439   2849   1569    784    309     40      3 \n\
                    Node 0, zone   Normal 235772  55229  20427  20958  11075   5958   2976   1190    285    296      3 \n";
        assert_eq!(free_huge_blocks(text), 1 + 4 + 40 + 6 + 296 + 6);
        assert_eq!(free_huge_blocks(""), 0);
    }
}
