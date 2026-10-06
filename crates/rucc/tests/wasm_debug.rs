//! What `-g` puts in a wasm object: the DWARF sections, as custom sections with their relocations,
//! in the object and in the `-S` text. With a wasi-sdk and Wasmtime on the machine, the module that
//! `wasm-ld` links from the object gives a backtrace with the file and the line of each frame, and
//! `llvm-dwarfdump --verify` takes the object and the module. The rucc linker writes the same
//! module as `wasm-ld`, byte for byte, with the same DWARF sections.
//!
//! Design: #3149.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A program that traps three calls deep. Each frame of the trap is on a line of its own.
const SOURCE: &str = "\
int depth(int n) {
    if (n == 0)
        __builtin_trap();
    return depth(n - 1) + 1;
}
int main(void) {
    return depth(2);
}
";

/// The sections that a reader needs to give the line of an address.
const SECTIONS: [&str; 5] =
    [".debug_line", ".debug_line_str", ".debug_abbrev", ".debug_info", ".debug_rnglists"];

/// A directory of its own for this test, under the temporary directory, with the source in it.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-wasm-debug-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("t.c"), SOURCE).unwrap();
    dir
}

fn rucc(args: &[&str], dir: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rucc"))
        .env("LC_ALL", "C")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("the compiler is built before its own tests run")
}

fn ok(out: &Output) {
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

/// Whether those bytes are somewhere in the file.
fn holds(bytes: &[u8], what: &str) -> bool {
    bytes.windows(what.len()).any(|window| window == what.as_bytes())
}

fn sdk() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("WASI_SDK_PATH")?);
    path.join("bin/wasm-ld").exists().then_some(path)
}

/// A program on `PATH`, or nothing.
fn on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(program)).find(|p| p.exists())
}

#[test]
fn asking_for_debug_information_writes_the_dwarf_sections() {
    let dir = scratch("shape");
    ok(&rucc(&["--target=wasm32-wasip1", "-g", "-c", "t.c", "-o", "with.o"], &dir));
    ok(&rucc(&["--target=wasm32-wasip1", "-c", "t.c", "-o", "without.o"], &dir));
    let with = std::fs::read(dir.join("with.o")).unwrap();
    let without = std::fs::read(dir.join("without.o")).unwrap();
    for name in SECTIONS {
        assert!(holds(&with, name), "a build with -g has no {name}");
    }
    // The line table and the unit hold the addresses of the functions, which the linker writes.
    for name in ["reloc..debug_line", "reloc..debug_info"] {
        assert!(holds(&with, name), "a build with -g has no {name}");
    }
    assert!(!holds(&without, ".debug_"), "a build without -g has debug sections");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn the_text_with_debug_information_assembles_to_the_same_object() {
    let dir = scratch("same");
    for target in ["wasm32-wasip1", "wasm32-wasip3", "wasm32-none"] {
        for level in ["-O0", "-O2"] {
            for version in ["-gdwarf-4", "-gdwarf-5"] {
                let target = format!("--target={target}");
                ok(&rucc(&[&target, level, version, "-S", "t.c", "-o", "t.s"], &dir));
                ok(&rucc(&[&target, level, version, "-c", "t.c", "-o", "c.o"], &dir));
                ok(&rucc(&[&target, "-c", "t.s", "-o", "s.o"], &dir));
                let text = std::fs::read_to_string(dir.join("t.s")).unwrap();
                assert!(text.contains("\t.section\t.debug_info,\"\",@\n"), "{text}");
                let (from_c, from_s) = (dir.join("c.o"), dir.join("s.o"));
                let same = std::fs::read(from_c).unwrap() == std::fs::read(from_s).unwrap();
                assert!(same, "{target} {level} {version}");
            }
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_trap_gives_the_line_of_each_frame() {
    let Some(sdk) = sdk() else {
        eprintln!("WASI_SDK_PATH is not set, so the module was not linked");
        return;
    };
    let dir = scratch("trap");
    ok(&rucc(&["--target=wasm32-wasip1", "-g", "-c", "t.c", "-o", "t.o"], &dir));
    ok(&rucc(&["--target=wasm32-wasip1", "-g", "t.o", "-o", "t.wasm"], &dir));
    // The name section holds the file name of the output, so the two modules have the same one.
    std::fs::create_dir_all(dir.join("own")).unwrap();
    ok(&rucc(&["--target=wasm32-wasip1", "-g", "-fuse-ld=rucc", "t.o", "-o", "own/t.wasm"], &dir));
    let same = std::fs::read(dir.join("t.wasm")).unwrap()
        == std::fs::read(dir.join("own/t.wasm")).unwrap();
    assert!(same, "the rucc linker and wasm-ld wrote different modules");
    let dwarfdump = sdk.join("bin/llvm-dwarfdump");
    if dwarfdump.exists() {
        for file in ["t.o", "t.wasm"] {
            let out = Command::new(&dwarfdump).arg("--verify").arg(dir.join(file)).output();
            let out = out.expect("llvm-dwarfdump starts");
            assert!(out.status.success(), "{file}: {}", String::from_utf8_lossy(&out.stdout));
        }
    }
    let Some(wasmtime) = on_path("wasmtime") else {
        eprintln!("there is no wasmtime on PATH, so the module was not run");
        std::fs::remove_dir_all(dir).unwrap();
        return;
    };
    let out = Command::new(wasmtime)
        .env("WASMTIME_BACKTRACE_DETAILS", "1")
        .arg(dir.join("t.wasm"))
        .output()
        .expect("wasmtime starts");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    // Cranelift traps at the `if` before the `unreachable`, so the frame of the trap is at the line
    // of the `if`, as it is for a module from clang.
    for frame in ["t.c:2:", "t.c:4:12", "t.c:7:12"] {
        assert!(stderr.contains(frame), "no frame at {frame}:\n{stderr}");
    }
    std::fs::remove_dir_all(dir).unwrap();
}
