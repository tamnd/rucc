//! The address of an array read with an index in a loop is taken once in front of the loop, which
//! is tamnd/rucc#3185.
//!
//! An address relative to a symbol has no room for an index, so `table[i]` is a `lea` of the table
//! and a load through it. The `lea` used to be written in every block that read the table, and in
//! a loop that is every trip. gcc 16 takes it once in front of the loop, and in front of the whole
//! nest when the loop is inside another, and so does rucc. A loop with a call in it is left the way
//! it was, since there the register would be one the callee saves.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, so the listing is the same on
/// every machine that runs the suite.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The shape of the `division` cases of rucc-corpus: a table filled in one loop and read in a nest.
const NEST: &str = r"
static unsigned long long table[2048];
unsigned long long sum(unsigned long long seed, int rounds)
{
  for (int i = 0; i < 2048; i++) {
    seed = seed * 6364136223846793005ull + 1442695040888963407ull;
    table[i] = seed;
  }
  unsigned long long total = 0;
  for (int round = 0; round < rounds; round++)
    for (int i = 0; i < 2048; i++)
      total += table[i] / 8;
  return total;
}
";

/// The same read, with a call on every trip.
const CALLING: &str = r"
void use(unsigned long long);
static unsigned long long table[2048];
void each(int n)
{
  for (int i = 0; i < n; i++)
    use(table[i * 3 % 2048]);
}
";

/// The fixture, written under a directory of its own so that two of these running at once do not
/// write the same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-name-address-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// The assembly the compiler writes for that source at that level.
fn compiled(what: &str, level: &str, source: &str) -> String {
    let path = fixture(&format!("{what}{level}"), source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args([level, "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(
        out.status.success(),
        "the compiler refused the fixture:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    String::from_utf8(out.stdout).expect("what the compiler writes is text")
}

/// The lines of the listing inside a loop, which is from a label to a later jump back to it.
fn in_loops(listing: &str) -> Vec<&str> {
    let lines: Vec<&str> = listing.lines().collect();
    let mut inside = vec![false; lines.len()];
    for (at, line) in lines.iter().enumerate() {
        let Some(target) = line.trim().strip_prefix('j').and_then(|rest| rest.split('\t').nth(1))
        else {
            continue;
        };
        let label = format!("{target}:");
        if let Some(start) = lines[..at].iter().position(|line| *line == label) {
            inside[start..at].iter_mut().for_each(|mark| *mark = true);
        }
    }
    lines.into_iter().zip(inside).filter(|&(_, inside)| inside).map(|(line, _)| line).collect()
}

/// How many times the table's address is taken inside a loop.
fn taken_in_loops(listing: &str) -> usize {
    in_loops(listing).iter().filter(|line| line.contains("table(%rip)")).count()
}

#[test]
fn the_table_s_address_is_taken_in_front_of_the_loops_at_each_level_above_o0() {
    for level in ["-O1", "-O2", "-O3"] {
        let listing = compiled("nest", level, NEST);
        assert!(!in_loops(&listing).is_empty(), "the listing at {level} has a loop:\n{listing}");
        assert_eq!(taken_in_loops(&listing), 0, "at {level}:\n{listing}");
        assert!(listing.contains("table(%rip)"), "the address is taken at {level}:\n{listing}");
    }
}

#[test]
fn a_loop_with_a_call_in_it_takes_the_address_on_each_trip_still() {
    let listing = compiled("calling", "-O2", CALLING);
    assert_eq!(taken_in_loops(&listing), 1, "{listing}");
}
