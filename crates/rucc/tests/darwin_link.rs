//! The two shapes Postgres links on a Mac besides its programs, linked and run on one.
//!
//! A module is a bundle, `-bundle -bundle_loader <program>`, whose undefined symbols are checked
//! against the program at link time and bound to it when the program opens the bundle with
//! `dlopen`. A library is `-dynamiclib` with the name it is found by, its versions and a list of what
//! it exports. Both are what `Makefile.shlib`, `Makefile.darwin` and `meson.build` write, and
//! both are checked here the way they are used: by loading them and calling through them.
//! tamnd/rucc#2010 and the `bundle` shape of tamnd/rucc#2009.
//!
//! Only on an arm64 Mac, since the linker, the SDK and the loader all have to be the real ones.
#![cfg(all(target_os = "macos", target_arch = "aarch64"))]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A directory of its own per test, so that two of these running at once do not share files.
fn scratch(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-darwin-link-{}-{what}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir
}

fn write(dir: &Path, name: &str, text: &str) {
    std::fs::write(dir.join(name), text).expect("a fixture can be written");
}

/// The compiler run in `dir` with those arguments, which has to succeed.
fn rucc(dir: &Path, args: &[&str]) {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(
        out.status.success(),
        "rucc {}:\n{}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A tool from the command line tools, run in `dir`.
fn tool(dir: &Path, name: &str, args: &[&str]) -> Output {
    Command::new(name)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("{name} could not be run: {e}"))
}

/// A program that opens a bundle and calls into it, and the bundle calls back into the program for
/// a function the bundle does not have, which is how every Postgres module reaches `palloc`.
#[test]
fn a_bundle_calls_back_into_the_program_that_loaded_it() {
    let dir = scratch("bundle");
    write(
        &dir,
        "host.c",
        "void *dlopen(const char *path, int mode);\n\
         void *dlsym(void *handle, const char *name);\n\
         char *dlerror(void);\n\
         int puts(const char *text);\n\
         int host_value(int x) { return x * 7; }\n\
         int main(int argc, char **argv) {\n\
             if (argc < 2) return 4;\n\
             void *handle = dlopen(argv[1], 2);\n\
             if (!handle) { puts(dlerror()); return 2; }\n\
             int (*entry)(int) = (int (*)(int))dlsym(handle, \"module_entry\");\n\
             if (!entry) { puts(dlerror()); return 3; }\n\
             return entry(6) == 43 ? 0 : 1;\n\
         }\n",
    );
    write(
        &dir,
        "module.c",
        "int host_value(int x);\n\
         int module_entry(int x) { return host_value(x) + 1; }\n",
    );
    rucc(&dir, &["-O1", "host.c", "-o", "host"]);
    rucc(&dir, &["-O1", "module.c", "-bundle", "-bundle_loader", "host", "-o", "module.so"]);

    // An `MH_BUNDLE`, which `otool -h` shows as file type 8.
    let header = tool(&dir, "otool", &["-hv", "module.so"]);
    let header = String::from_utf8_lossy(&header.stdout).into_owned();
    assert!(header.contains("BUNDLE"), "{header}");

    let run = tool(&dir, "./host", &["./module.so"]);
    assert!(
        run.status.success(),
        "the program exited with {:?}: {}",
        run.status.code(),
        String::from_utf8_lossy(&run.stdout)
    );

    // And a bundle that needs something the program does not have is a link error, which is what
    // `-bundle_loader` is for.
    write(&dir, "wrong.c", "int not_in_host(int);\nint f(int x) { return not_in_host(x); }\n");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["wrong.c", "-bundle", "-bundle_loader", "host", "-o", "wrong.so"])
        .current_dir(&dir)
        .output()
        .expect("the compiler runs");
    assert!(!out.status.success(), "a bundle with a symbol nothing defines was linked");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A library with an install name under `@rpath`, two versions and an export list, linked into a
/// program that finds it through a run path given on the compiler line.
#[test]
fn a_dynamic_library_has_its_name_versions_and_only_its_exports() {
    let dir = scratch("dylib");
    write(
        &dir,
        "lib.c",
        "int lib_hidden(int x) { return x + 1; }\n\
         int lib_kept(int x) { return lib_hidden(x) + 99; }\n",
    );
    write(&dir, "exports.list", "_lib_kept\n");
    write(
        &dir,
        "main.c",
        "int lib_kept(int x);\nint main(void) { return lib_kept(5) == 105 ? 0 : 1; }\n",
    );
    rucc(
        &dir,
        &[
            "-O1",
            "-dynamiclib",
            "-install_name",
            "@rpath/libx.dylib",
            "-compatibility_version",
            "1",
            "-current_version",
            "1.2",
            "-exported_symbols_list",
            "exports.list",
            "-headerpad_max_install_names",
            "lib.c",
            "-o",
            "libx.dylib",
        ],
    );
    let here = dir.display().to_string();
    rucc(
        &dir,
        &["-O1", "main.c", "-L.", "-lx", "-rpath", &here, "-Wl,-dead_strip_dylibs", "-o", "prog"],
    );

    let name = tool(&dir, "otool", &["-D", "libx.dylib"]);
    let name = String::from_utf8_lossy(&name.stdout).into_owned();
    assert!(name.contains("@rpath/libx.dylib"), "{name}");
    let loads = tool(&dir, "otool", &["-L", "libx.dylib"]);
    let loads = String::from_utf8_lossy(&loads.stdout).into_owned();
    assert!(loads.contains("compatibility version 1.0.0, current version 1.2.0"), "{loads}");
    let exported = tool(&dir, "nm", &["-gU", "libx.dylib"]);
    let exported = String::from_utf8_lossy(&exported.stdout).into_owned();
    assert!(exported.contains("_lib_kept"), "{exported}");
    assert!(!exported.contains("_lib_hidden"), "{exported}");

    let run = tool(&dir, "./prog", &[]);
    assert!(run.status.success(), "the program exited with {:?}", run.status.code());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A program built with `-g` whose `.dSYM` places every function where the linker put it. Each
/// address in the debug information of an object holds where its function is in that object, and
/// `dsymutil` moves it by the distance the linker moved the function, so an object that held only
/// the addend put every function at the first one's address and a debugger or `atos` named the
/// wrong function and the wrong line for all of them but the first (#1992).
#[test]
fn the_debug_map_places_every_function_where_the_linker_put_it() {
    let dir = scratch("dsym");
    write(
        &dir,
        "one.c",
        "int first(int x) { return x + 1; }\n\
         int second(int x) { return first(x) * 2; }\n\
         int main(void) { return second(1) == 4 ? 0 : 1; }\n",
    );
    rucc(&dir, &["-g", "-O0", "-c", "one.c", "-o", "one.o"]);
    rucc(&dir, &["one.o", "-o", "prog"]);
    let run = tool(&dir, "./prog", &[]);
    assert!(run.status.success(), "the program exited with {:?}", run.status.code());
    let made = tool(&dir, "dsymutil", &["prog"]);
    assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));

    let names = tool(&dir, "nm", &["prog"]);
    let names = String::from_utf8_lossy(&names.stdout).into_owned();
    for (symbol, function, line) in [("_second", "second", 2), ("_main", "main", 3)] {
        let address = names
            .lines()
            .find_map(|row| row.strip_suffix(&format!(" T {symbol}")))
            .unwrap_or_else(|| panic!("no {symbol} in\n{names}"));
        let found = tool(&dir, "dwarfdump", &[&format!("--lookup=0x{address}"), "prog.dSYM"]);
        let found = String::from_utf8_lossy(&found.stdout).into_owned();
        assert!(found.contains(&format!("DW_AT_name\t(\"{function}\")")), "{found}");
        assert!(found.contains(&format!("file 'one.c', line {line},")), "{found}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
