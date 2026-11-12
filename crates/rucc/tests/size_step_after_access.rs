//! At `-Os` and `-Oz` the step of a pointer goes after the access that reads the old pointer, so
//! the two share a register and the step is one `inc`. FreeBSD's `boot2` copies with this loop in
//! its `memcpy`, and it has to fit in 7680 bytes.

use std::process::Command;

const SOURCE: &str = "\
void copy(char *d, const char *s, int n) {
  while (n--)
    *d++ = *s++;
}
";

fn assembly(name: &str, target: &str, level: &str, extra: &[&str]) -> String {
    let dir = std::env::temp_dir().join(format!("rucc-step-sink-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([&format!("--target={target}"), "-fno-pic", level, "-S", "-o", "-"])
        .args(extra)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{name}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("assembly is text")
}

/// The instructions of the function, without labels and directives.
fn instructions(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|line| line.starts_with('\t') && !line.trim_start().starts_with('.'))
        .map(str::trim)
        .collect()
}

#[test]
fn the_steps_of_a_copy_loop_are_one_inc_each_for_size() {
    for target in ["i686-unknown-linux-gnu", "x86_64-unknown-linux-gnu"] {
        for level in ["-Os", "-Oz"] {
            let name = format!("{level}-{target}");
            let text = assembly(&name, target, level, &[]);
            let with = instructions(&text);
            let off = assembly(&format!("{name}-off"), target, level, &["-fno-step-sink"]);
            let without = instructions(&off);
            let incs = with.iter().filter(|inst| inst.starts_with("inc")).count();
            assert_eq!(incs, 2, "{name}: each pointer steps with one inc\n{text}");
            assert!(
                with.len() < without.len(),
                "{name}: {} instructions, {} without the pass\n{text}\n{off}",
                with.len(),
                without.len()
            );
        }
    }
}
