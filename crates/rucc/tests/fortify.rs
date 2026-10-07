//! What `_FORTIFY_SOURCE` does at run time, on a Linux host with glibc.
//!
//! The other tests read the calls to the `_chk` functions in the object. This one runs the
//! program. Each case writes past an 8-byte buffer through one glibc function, and glibc must stop
//! it with the message that it gives for a GCC build. The same case with a short write must run to
//! the end. Issue #1538 said that fortify had no effect. The cases here are the evidence that it
//! now has.

#![cfg(all(target_os = "linux", target_env = "gnu"))]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// One write for each case, chosen by the first argument. The second argument is the length.
const OVERFLOW: &str = r#"
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

int main(int argc, char **argv) {
    char b[8];
    int n = atoi(argv[2]);
    const char *what = argv[1];
    char big[64];
    memset(big, 'x', sizeof big - 1);
    big[63] = 0;
    if (!strcmp(what, "memcpy")) memcpy(b, big, n);
    else if (!strcmp(what, "memset")) memset(b, 0, n);
    else if (!strcmp(what, "strcpy")) strcpy(b, big + 63 - n);
    else if (!strcmp(what, "strcat")) { b[0] = 0; strcat(b, big + 63 - n); }
    else if (!strcmp(what, "sprintf")) sprintf(b, "%s", big + 63 - n);
    else if (!strcmp(what, "snprintf")) snprintf(b, n, "%s", big);
    else if (!strcmp(what, "read")) { int fd = open("/dev/zero", O_RDONLY); if (read(fd, b, n) < 0) return 3; }
    else if (!strcmp(what, "fgets")) { FILE *f = fopen("/dev/zero", "r"); if (!fgets(b, n, f)) return 3; }
    else if (!strcmp(what, "percentn")) { char fmt[8]; int x = 0; strcpy(fmt, "ab%n"); printf(fmt, &x); }
    else if (!strcmp(what, "open")) { int fl = argc > 3 ? 0 : O_CREAT | O_WRONLY; open("/nonexistent/rucc-fortify", fl); }
    puts("ran");
    return 0;
}
"#;

/// A buffer with a size that is known only at run time. Level 3 checks it and level 2 does not.
const DYNAMIC: &str = r#"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int main(int argc, char **argv) {
    size_t size = strtoul(argv[1], 0, 10), n = strtoul(argv[2], 0, 10);
    char big[64];
    memset(big, 'x', sizeof big);
    char *heap = malloc(size);
    memcpy(heap, big, n);
    char vla[size];
    memcpy(vla, big, n);
    printf("ran %d\n", heap[0] + vla[0]);
    return argc > 3;
}
"#;

/// The message from glibc for a write past the end of a buffer.
const OVERFLOWED: &str = "buffer overflow detected";

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-fortify-{}-{what}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir
}

/// Compiles that source at `-O2` with that fortify level, and gives what the compiler said.
fn compile(dir: &Path, source: &str, level: u32, out: &str) -> Output {
    std::fs::write(dir.join("a.c"), source).expect("the fixture can be written");
    Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["-O2", &format!("-D_FORTIFY_SOURCE={level}"), "a.c", "-o", out])
        .current_dir(dir)
        .output()
        .expect("the compiler is built before its own tests run")
}

/// Builds the program and stops the test if the compiler refused it.
fn build(dir: &Path, source: &str, level: u32) -> PathBuf {
    let name = format!("prog{level}");
    let out = compile(dir, source, level, &name);
    assert!(out.status.success(), "level {level}: {}", String::from_utf8_lossy(&out.stderr));
    dir.join(name)
}

/// Runs the program, and gives whether it ran to the end and all that it wrote.
fn run(program: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(program).args(args).output().expect("what was linked can be run");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), text)
}

/// Each function stops a long write and lets a short one through, at both levels.
#[test]
fn a_write_past_the_buffer_stops_the_program() {
    let dir = dir("overflow");
    for level in [2, 3] {
        let program = build(&dir, OVERFLOW, level);
        for case in ["memcpy", "memset", "strcpy", "strcat", "sprintf", "snprintf", "read", "fgets"]
        {
            let (ok, said) = run(&program, &[case, "4"]);
            assert!(ok && said.contains("ran"), "level {level}, {case} 4: {said}");
            let (ok, said) = run(&program, &[case, "16"]);
            assert!(!ok && said.contains(OVERFLOWED), "level {level}, {case} 16: {said}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The two checks that are not about a size: `%n` in a format that the program can write to, and
/// `open` with `O_CREAT` and no mode when the flags are known only at run time.
#[test]
fn a_writable_percent_n_and_an_open_with_no_mode_stop_the_program() {
    let dir = dir("other");
    for level in [2, 3] {
        let program = build(&dir, OVERFLOW, level);
        let (ok, said) = run(&program, &["percentn", "0"]);
        assert!(!ok && said.contains("%n in writable segment"), "level {level}: {said}");
        let (ok, said) = run(&program, &["open", "0"]);
        assert!(!ok && said.contains("O_CREAT or O_TMPFILE without mode"), "level {level}: {said}");
        let (ok, said) = run(&program, &["open", "0", "no-create"]);
        assert!(ok && said.contains("ran"), "level {level}: {said}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Level 3 uses `__builtin_dynamic_object_size`, so it knows the size of a `malloc` buffer and of
/// a VLA. Level 2 does not, and the write goes through, as with GCC.
#[test]
fn level_3_checks_a_buffer_with_a_size_known_at_run_time() {
    let dir = dir("dynamic");
    let two = build(&dir, DYNAMIC, 2);
    let three = build(&dir, DYNAMIC, 3);
    for program in [&two, &three] {
        let (ok, said) = run(program, &["8", "4"]);
        assert!(ok && said.contains("ran"), "{said}");
    }
    // 16 bytes still fit in what `malloc(8)` and the aligned VLA really have, so the program at
    // level 2 runs to the end. Only the size that the program asked for is smaller.
    let (ok, said) = run(&two, &["8", "16"]);
    assert!(ok && said.contains("ran"), "level 2 has no size for the buffer: {said}");
    let (ok, said) = run(&three, &["8", "16"]);
    assert!(!ok && said.contains(OVERFLOWED), "{said}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// With the flags known when it compiles, the `open` check is an error from the compiler.
#[test]
fn an_open_with_no_mode_is_an_error_when_the_flags_are_constant() {
    let dir = dir("open");
    let source = "#include <fcntl.h>\nint main(void) { return open(\"x\", O_CREAT | O_WRONLY); }\n";
    let out = compile(&dir, source, 2, "prog");
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{said}");
    assert!(said.contains("__open_missing_mode"), "{said}");
    let _ = std::fs::remove_dir_all(&dir);
}
