//! A kernel before 5.15 builds with `-nostdinc -isystem $(CC -print-file-name=include)` and takes
//! `<stdarg.h>` from that directory, so the answer has to be one a shell can pass on.

use std::process::Command;

#[test]
fn the_include_directory_printed_is_one_nostdinc_can_read_stdarg_from() {
    let cache = std::env::temp_dir().join(format!("rucc-print-include-{}", std::process::id()));
    let rucc = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_rucc"));
        command.env("RUCC_CACHE_DIR", &cache);
        command
    };
    let out = rucc().arg("-print-file-name=include").output().unwrap();
    let dir = String::from_utf8(out.stdout).unwrap().trim().to_owned();
    assert!(std::path::Path::new(&dir).join("stdarg.h").is_file(), "{dir}");
    let source = cache.join("va.c");
    std::fs::write(
        &source,
        "#include <stdarg.h>\nint f(int n, ...) { va_list ap; va_start(ap, n); n = va_arg(ap, int); va_end(ap); return n; }\n",
    )
    .unwrap();
    let out = rucc()
        .args(["--target=x86_64-unknown-linux-gnu", "-nostdinc", "-isystem", &dir, "-fsyntax-only"])
        .arg(&source)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    std::fs::remove_dir_all(&cache).unwrap();
}
