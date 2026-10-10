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
    let dir = dir.canonicalize().expect("the directory is there");
    // On Windows that is a verbatim path, `\\?\C:\...`, and the compiler's working directory is
    // the same place without the prefix, which is the one it writes into what it makes.
    PathBuf::from(dir.display().to_string().trim_start_matches(r"\\?\"))
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

/// The runtime half, with the record laid out the way gcc 15 and later lay it out, and the
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
  void *merge[10]; unsigned n; struct fn **fns;
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
        // The release is written, because the program brings its own runtime. A line that does
        // not say one takes the release of the `libgcov.a` on the machine.
        let args = [level, "-fgnuc-version=16", "-fprofile-arcs", "a.c", "-o", "prog"];
        let (ok, said) = run(&dir, &args);
        assert!(ok, "{level}: {said}");
        let out = Command::new(dir.join("prog")).output().expect("what was linked can be run");
        assert!(out.status.success(), "{level}: the program got the wrong answer");
        // `B60*` is gcc 16.0, which is the release the line says. `work` runs
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
    let full = dir.join("out/x.gcda").display().to_string();
    // Beside the object and absolute, so that the program writes it there from wherever it runs.
    named(&[], "out/x.o", &full);
    // Under the directory, with the whole path folded into one name so that two objects called
    // `x.o` in different places keep apart. On Windows the drive's colon is folded as well.
    let folded = full.replace(['/', '\\'], "#").replacen(':', "~", 1);
    named(
        &["-fprofile-dir=counts"],
        "out/x.o",
        &dir.join("counts").join(folded).display().to_string(),
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

/// A program for `gcov`, with a line run ten times and a line never run.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const COVERED: &str = "\
int twice(int x) {
    return x * 2;
}
int main(void) {
    int s = 0;
    for (int i = 0; i < 10; i++)
        s += twice(i);
    if (s == 0)
        return 1;
    return 0;
}
";

/// A program built with `--coverage` and linked with the `libgcov.a` of the GCC on the machine
/// writes its counts, and `gcov` reads them. The record has to have the release of that library,
/// or the library writes nothing. Left out on a machine with no `gcov`.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn gcov_reads_the_counts_of_a_covered_program() {
    if Command::new("gcov").arg("--version").output().is_err() {
        return;
    }
    let dir = dir("gcov");
    std::fs::write(dir.join("b.c"), COVERED).expect("the fixture can be written");
    let (ok, said) = run(&dir, &["--coverage", "-c", "b.c", "-o", "b.o"]);
    assert!(ok, "{said}");
    let (ok, said) = run(&dir, &["--coverage", "b.o", "-o", "prog"]);
    assert!(ok, "{said}");
    let out = Command::new(dir.join("prog")).output().expect("what was linked can be run");
    assert!(out.status.success(), "the program got the wrong answer");
    assert!(
        dir.join("b.gcda").exists(),
        "no counts were written: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = Command::new("gcov").arg("b.c").current_dir(&dir).output().expect("gcov starts");
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{said}");
    let listing = std::fs::read_to_string(dir.join("b.c.gcov")).expect("gcov wrote a listing");
    // The body of `twice` ran ten times, and the early return never ran.
    assert!(listing.contains("10:    2:"), "{listing}");
    assert!(listing.contains("#####:    9:"), "{listing}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The 32-bit words of a `.gcno` file from where it starts, in the order the host wrote them.
fn words(bytes: &[u8]) -> Vec<u32> {
    bytes.chunks_exact(4).map(|w| u32::from_ne_bytes([w[0], w[1], w[2], w[3]])).collect()
}

#[test]
fn coverage_writes_the_note_beside_the_object_and_test_coverage_alone_counts_nothing() {
    let dir = dir("notes");
    std::fs::create_dir_all(dir.join("out")).expect("a directory can be made");
    std::fs::write(dir.join("a.c"), "int f(int x) {\n  return x ? 1 : 2;\n}\n")
        .expect("the fixture can be written");
    let target = "--target=x86_64-unknown-linux-gnu";
    let (ok, said) =
        run(&dir, &[target, "-fgnuc-version=13.3", "--coverage", "-c", "a.c", "-o", "out/x.o"]);
    assert!(ok, "{said}");
    let note = std::fs::read(dir.join("out/x.gcno")).expect("the note is beside the object");
    // The magic, gcc 13.3's version word, the stamp, a checksum of zero, and then the working
    // directory as a length in bytes and the bytes, which is what gcov 13 reads.
    let head = words(&note[..20]);
    assert_eq!(head[0], 0x6763_6e6f);
    assert_eq!(head[1].to_be_bytes(), *b"B33*");
    assert_eq!(head[3], 0);
    let cwd = dir.display().to_string();
    assert_eq!(head[4] as usize, cwd.len() + 1);
    assert_eq!(&note[20..20 + cwd.len()], cwd.as_bytes());
    // The function's record, by its name and the file it is in.
    for name in [&b"f\0"[..], b"a.c\0"] {
        assert!(note.windows(name.len()).any(|w| w == name), "the note names {name:?}");
    }
    // It starts at the name, line 1 column 5, and ends at the closing brace on line 3, as gcc
    // says.
    let place: Vec<u8> = [1_u32, 5, 3, 1].iter().flat_map(|w| w.to_ne_bytes()).collect();
    assert!(note.windows(16).any(|w| w == place), "the function is not placed from 1:5 to 3:1");
    // The stamp is the one the record in the object holds, which is how gcov pairs them.
    let object = std::fs::read(dir.join("out/x.o")).expect("the object was written");
    assert!(object.windows(4).any(|w| w == head[2].to_ne_bytes()), "the stamps differ");

    // The graph without the counters.
    let (ok, said) = run(&dir, &[target, "-ftest-coverage", "-c", "a.c", "-o", "out/y.o"]);
    assert!(ok, "{said}");
    assert!(dir.join("out/y.gcno").exists(), "no note");
    let object = std::fs::read(dir.join("out/y.o")).expect("the object was written");
    let init = b"__gcov_init";
    assert!(!object.windows(init.len()).any(|w| w == init), "nothing is counted");
    let _ = std::fs::remove_dir_all(&dir);
}
