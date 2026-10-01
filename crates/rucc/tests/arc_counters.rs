//! `-fprofile-arcs`, run rather than inspected: the counters a program built with it hands to
//! `__gcov_init`, and the `.gcda` name the record holds.
//!
//! The program brings its own `__gcov_init`, which keeps the record and prints every counter when
//! the program ends. That is the job libgcov and the kernel's `gcov` do with the same record, and
//! it is how this can check the counts without a gcc on the machine.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-arcs-{}-{what}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir.canonicalize().expect("the directory is there")
}

/// Whether the compiler finished, and what it said.
fn run(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("the compiler is built before its own tests run");
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// The runtime half, with the record laid out the way gcc 14 and later lay it out, and the
/// program it counts. `skip` is taken out by its attribute and the runtime takes itself out the
/// same way, so the listing is the three functions below it.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const PROGRAM: &str = r#"
#include <stdio.h>
#define QUIET __attribute__((no_profile_instrument_function))
struct ctr { unsigned num; long long *values; };
struct fn { void *key; unsigned ident, lineno, cfg; struct ctr c[1]; };
struct info {
  unsigned version; struct info *next; unsigned stamp, checksum; const char *filename;
  void *merge[9]; unsigned n; struct fn **fns;
};
static struct info *head;
QUIET void __gcov_merge_add(void *p, unsigned n) {}
QUIET void __gcov_exit(void) {
  for (struct info *i = head; i; i = i->next) {
    printf("%08x %u\n", i->version, i->n);
    for (unsigned f = 0; f < i->n; f++) {
      struct fn *fn = i->fns[f];
      printf("%u", fn->ident);
      for (unsigned k = 0; k < fn->c[0].num; k++) printf(" %lld", fn->c[0].values[k]);
      printf("\n");
    }
  }
}
QUIET void __gcov_init(struct info *i) { i->next = head; head = i; }
QUIET int skip(int x) { return x ? x * 2 : 1; }
int work(int n) {
  int s = 0;
  for (int i = 0; i < n; i++) { if (i & 1) s += i; else s -= 1; }
  return s;
}
int pick(int x) { switch (x) { case 1: return 10; case 2: return 20; case 3: return 30; default: return 0; } }
int main(void) {
  int r = work(10) + work(3) + skip(1);
  for (int i = 0; i < 5; i++) r += pick(i);
  return r != 81;
}
"#;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn a_counted_program_hands_its_counts_to_gcov_init() {
    let dir = dir("run");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    for level in ["-O0", "-O2"] {
        let (ok, said) = run(&dir, &[level, "-fprofile-arcs", "a.c", "-o", "prog"]);
        assert!(ok, "{level}: {said}");
        let out = Command::new(dir.join("prog")).output().expect("what was linked can be run");
        assert!(out.status.success(), "{level}: the program got the wrong answer");
        // `B60*` is gcc 16.0, which is what a line that does not say is taken to be. `work` runs
        // twice and goes round its loop thirteen times, seven of them on the odd branch. `pick`
        // takes the default twice and each case once, and `main` is entered once and goes round
        // five times. Every other edge is the sum of these, which is why it has no counter.
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "4236302a 3\n1 2 7 13\n2 2 1 1 1\n3 1 5\n",
            "{level}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_record_names_the_counts_file_the_way_gcc_does() {
    let dir = dir("names");
    std::fs::create_dir_all(dir.join("out")).expect("a directory can be made");
    std::fs::write(dir.join("a.c"), "int f(int x) { return x ? 1 : 2; }\n")
        .expect("the fixture can be written");
    let target = "--target=x86_64-unknown-linux-gnu";
    let named = |flags: &[&str], object: &str, want: &str| {
        let mut args = vec![target, "-fprofile-arcs", "-c", "a.c", "-o", object];
        args.extend_from_slice(flags);
        let (ok, said) = run(&dir, &args);
        assert!(ok, "{flags:?}: {said}");
        let bytes = std::fs::read(dir.join(object)).expect("the object was written");
        let want = format!("{want}\0");
        assert!(
            bytes.windows(want.len()).any(|w| w == want.as_bytes()),
            "{flags:?}: the object does not name {want}"
        );
    };
    let base = dir.display().to_string();
    // Beside the object and absolute, so that the program writes it there from wherever it runs.
    named(&[], "out/x.o", &format!("{base}/out/x.gcda"));
    // Under the directory, with the whole path folded into one name so that two objects called
    // `x.o` in different places keep apart.
    named(
        &["-fprofile-dir=counts"],
        "out/x.o",
        &format!("{base}/counts/{}", format!("{base}/out/x.gcda").replace('/', "#")),
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn turning_the_counters_back_off_gives_the_object_nobody_asked_for() {
    let dir = dir("off");
    std::fs::write(dir.join("a.c"), "int f(int x) { return x ? 1 : 2; }\n")
        .expect("the fixture can be written");
    let target = "--target=x86_64-unknown-linux-gnu";
    let (ok, said) = run(&dir, &[target, "-c", "a.c", "-o", "plain.o"]);
    assert!(ok, "{said}");
    let (ok, said) =
        run(&dir, &[target, "-fprofile-arcs", "-fno-profile-arcs", "-c", "a.c", "-o", "off.o"]);
    assert!(ok, "{said}");
    let (ok, said) = run(&dir, &[target, "-fprofile-arcs", "-c", "a.c", "-o", "on.o"]);
    assert!(ok, "{said}");
    let read = |name: &str| std::fs::read(dir.join(name)).expect("the object was written");
    assert_eq!(read("off.o"), read("plain.o"));
    assert_ne!(read("on.o"), read("plain.o"));
    let on = read("on.o");
    for name in ["__gcov_init", "__gcov_merge_add", "__gcov0.f", ".init_array.00101"] {
        assert!(on.windows(name.len()).any(|w| w == name.as_bytes()), "no {name} in the object");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
