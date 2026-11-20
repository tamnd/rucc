//! Running the jobs in a [`Plan`](crate::Plan) across threads, deterministically.
//!
//! Design: `spec/03-architecture.md` section 3.5, and section 3.7 for the determinism rule.
//!
//! `rucc a.c b.c c.c` compiles all three in one process on a shared
//! [`Session`](rucc_session::Session), rather than the build system forking three processes
//! that each re-read every header. That is the larger of the two levels of parallelism and it
//! is the reason `Session` is thread-safe rather than merely convenient.
//!
//! The rule that makes this safe to have at all: **each unit of parallel work writes only to
//! its own slot, and results are merged in index order, never in completion order.** Byte
//! identical output is a requirement in `spec/02-the-goal.md`, and a scheduler that merges by
//! whoever finishes first quietly gives it up. The API here makes that hard to get wrong,
//! because the only thing a caller can do with a result is receive the whole vector back in
//! input order.
//!
//! # Status
//!
//! The scheduler is real. The work it schedules is not, until M3. `spec/18-package-layout.md`
//! section 18.3 has `rayon` down for this, and it will be needed for the per-function level
//! inside a translation unit; for the per-file level a scoped thread per job is the whole
//! implementation and it costs no dependency, so that is what this is.

use std::num::NonZeroUsize;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// How many jobs to run at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Jobs {
    /// One job at a time, in order. What `-j1` asks for, and what the determinism check in
    /// CI compares against.
    Serial,
    /// At most this many at once.
    Threads(NonZeroUsize),
}

impl Default for Jobs {
    fn default() -> Jobs {
        Jobs::available()
    }
}

impl Jobs {
    /// What the machine can do, or serial when it will not say.
    #[must_use]
    pub fn available() -> Jobs {
        match std::thread::available_parallelism() {
            Ok(n) if n.get() > 1 => Jobs::Threads(n),
            _ => Jobs::Serial,
        }
    }

    /// The number of workers this asks for.
    #[must_use]
    pub fn count(self) -> usize {
        match self {
            Jobs::Serial => 1,
            Jobs::Threads(n) => n.get(),
        }
    }

    /// Parses the argument of `-j`.
    ///
    /// A bare `-j` with no number means "as many as the machine has", which is what `make`
    /// does and therefore what people expect.
    ///
    /// # Errors
    ///
    /// Returns the offending text when it is not a positive number.
    pub fn parse(arg: &str) -> Result<Jobs, String> {
        if arg.is_empty() {
            return Ok(Jobs::available());
        }
        match arg.parse::<NonZeroUsize>() {
            // One worker is the serial path, not a pool of one, so that `-j1` is exactly what
            // the determinism check in CI compares against rather than merely equivalent.
            Ok(n) if n.get() == 1 => Ok(Jobs::Serial),
            Ok(n) => Ok(Jobs::Threads(n)),
            Err(_) if arg == "0" => Err("-j0 asks for no workers at all".to_owned()),
            Err(_) => Err(format!("`{arg}` is not a job count")),
        }
    }
}

/// Runs `work` over every item, and returns the results in input order.
///
/// The closure runs on several threads at once and may finish in any order. The vector that
/// comes back does not depend on that order, on the thread count, or on timing, which is the
/// property `spec/03-architecture.md` section 3.7 requires and the reason this function
/// exists rather than each caller reaching for threads directly.
///
/// A panic in the closure propagates once every other job has finished, rather than leaving
/// the compilation half done with no diagnostic.
///
/// # Panics
///
/// Panics if `work` panicked on any item.
pub fn run<T, R, F>(jobs: Jobs, items: &[T], work: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(usize, &T) -> R + Sync,
{
    if items.is_empty() {
        return Vec::new();
    }
    let workers = jobs.count().min(items.len());
    if workers <= 1 {
        return items.iter().enumerate().map(|(i, t)| work(i, t)).collect();
    }

    // One slot per item, so no two threads ever write to the same place and the merge is
    // just "take the slots in order". A channel would have been shorter and would have made
    // the output depend on completion order.
    let slots: Vec<Mutex<Option<R>>> = items.iter().map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);

    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    // Claiming the next index rather than splitting the work up front, because
                    // translation units differ in size by more than an order of magnitude and
                    // a static split leaves most of the machine idle behind the biggest file.
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else { break };
                    let result = work(i, item);
                    *slots[i].lock().expect("a slot lock is only held to store one result") =
                        Some(result);
                }
            });
        }
    });

    slots
        .into_iter()
        .map(|slot| {
            slot.into_inner()
                .expect("a slot lock is only held to store one result")
                .expect("every index was claimed exactly once")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    #[test]
    fn results_come_back_in_input_order_however_they_finish() {
        // The first job is the slowest, so completion order is the reverse of input order.
        // If the merge depended on completion order this test would fail, and so would the
        // determinism check in CI, but much later and much less clearly.
        let items: Vec<u64> = (0..8).collect();
        let out = run(Jobs::Threads(NonZeroUsize::new(8).unwrap()), &items, |i, x| {
            std::thread::sleep(std::time::Duration::from_millis((8 - i as u64) * 4));
            x * 10
        });
        assert_eq!(out, vec![0, 10, 20, 30, 40, 50, 60, 70]);
    }

    #[test]
    fn serial_and_parallel_give_the_same_answer() {
        let items: Vec<usize> = (0..64).collect();
        let serial = run(Jobs::Serial, &items, |i, x| i + x);
        let parallel = run(Jobs::Threads(NonZeroUsize::new(4).unwrap()), &items, |i, x| i + x);
        assert_eq!(serial, parallel);
    }

    #[test]
    fn every_item_runs_exactly_once() {
        let items: Vec<usize> = (0..500).collect();
        let calls = AtomicUsize::new(0);
        let out = run(Jobs::Threads(NonZeroUsize::new(16).unwrap()), &items, |_, x| {
            calls.fetch_add(1, Ordering::Relaxed);
            *x
        });
        assert_eq!(calls.load(Ordering::Relaxed), 500);
        assert_eq!(out, items);
    }

    #[test]
    fn more_workers_than_items_is_fine() {
        let items = [1, 2];
        let out = run(Jobs::Threads(NonZeroUsize::new(64).unwrap()), &items, |_, x| *x);
        assert_eq!(out, vec![1, 2]);
    }

    #[test]
    fn no_items_is_no_threads_and_no_results() {
        let items: [u8; 0] = [];
        let out: Vec<u8> = run(Jobs::available(), &items, |_, x| *x);
        assert!(out.is_empty());
    }

    #[test]
    fn dash_j_reads_the_way_make_reads_it() {
        assert_eq!(Jobs::parse("1").unwrap(), Jobs::Serial);
        assert_eq!(Jobs::parse("4").unwrap(), Jobs::Threads(NonZeroUsize::new(4).unwrap()));
        assert_eq!(Jobs::parse("").unwrap(), Jobs::available());
        assert!(Jobs::parse("0").is_err());
        assert!(Jobs::parse("many").is_err());
    }

    #[test]
    fn a_panicking_job_is_not_swallowed() {
        // A compiler that loses an internal error and exits zero is worse than one that
        // crashes, because the build carries on with a missing object.
        let items = [0, 1, 2];
        let r = std::panic::catch_unwind(|| {
            run(Jobs::Threads(NonZeroUsize::new(3).unwrap()), &items, |_, x| {
                assert!(*x != 1, "planted failure");
                *x
            })
        });
        assert!(r.is_err());
    }
}
