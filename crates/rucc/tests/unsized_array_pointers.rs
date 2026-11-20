//! A pointer to an array of unknown length, dereferenced and then indexed.
//!
//! `*p` is an array whose length nobody knows, which decays to a pointer to its first element
//! like any other array, so `(*p)[i]` is fine. Only measuring the array or stepping over one is
//! refused, as in gcc. Landlock passes its rules around as `const struct landlock_layer (*)[]`.

use std::process::Command;

const TARGET: &str = "x86_64-linux-gnu";

fn check(name: &str, source: &str) -> (bool, String) {
    let dir = std::env::temp_dir().join(format!("rucc-unsized-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(["-fsyntax-only", "-Werror"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn an_array_of_unknown_length_behind_a_pointer_can_be_indexed() {
    let (ok, stderr) = check(
        "index",
        "struct layer { unsigned short level, access; };
int f(const struct layer (*layers)[], unsigned short (*masks)[]) {
    if ((*layers)[0].level == 0)
        return (*masks)[1];
    return sizeof((*layers)[0]);
}
",
    );
    assert!(ok, "{stderr}");
    assert!(stderr.is_empty(), "{stderr}");
}

#[test]
fn an_array_of_unknown_length_still_cannot_be_measured_or_stepped_over() {
    let (ok, stderr) = check("measure", "int g(int (*p)[]) { return sizeof(*p); }\n");
    assert!(!ok);
    assert!(stderr.contains("incomplete type 'int[]'"), "{stderr}");
    let (ok, stderr) = check("step", "int h(int (*p)[]) { return p[0][1]; }\n");
    assert!(!ok);
    assert!(stderr.contains("undefined type 'int[]'"), "{stderr}");
}
