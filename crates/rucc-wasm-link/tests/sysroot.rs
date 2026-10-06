//! The readers on the libraries of wasi-sdk, when `WASI_SDK_PATH` names one. Every member of
//! every archive and every `crt1` object must parse, because a link reads all of them.

use std::path::PathBuf;

use rucc_wasm_link::{archive, object::Object};

fn sysroot() -> Option<PathBuf> {
    let sdk = PathBuf::from(std::env::var_os("WASI_SDK_PATH")?);
    let lib = sdk.join("share/wasi-sysroot/lib/wasm32-wasip1");
    lib.is_dir().then_some(lib)
}

#[test]
fn every_object_of_the_wasip1_sysroot_parses() {
    let Some(lib) = sysroot() else {
        eprintln!("WASI_SDK_PATH is not set, so there is no sysroot to read");
        return;
    };
    let mut objects = 0;
    for file in ["libc.a", "libm.a", "libsetjmp.a", "crt1-command.o", "crt1-reactor.o"] {
        let bytes = std::fs::read(lib.join(file)).unwrap();
        if archive::is_archive(&bytes) {
            // `libm.a` has no members, because `libc.a` has the math functions.
            for member in archive::members(&bytes).unwrap() {
                let name = format!("{file}({})", member.name);
                Object::parse(&name, member.bytes).unwrap_or_else(|e| panic!("{e}"));
                objects += 1;
            }
        } else {
            Object::parse(file, &bytes).unwrap_or_else(|e| panic!("{e}"));
            objects += 1;
        }
    }
    assert!(objects > 800, "{objects}");
}

/// A reactor of the C library with `malloc` and `free` exported: what the link fetches from
/// `libc.a` must resolve, and what nothing reaches must not be in the module.
#[test]
fn a_reactor_links_against_the_wasip1_sysroot() {
    use rucc_wasm_link::{Input, Options, link};
    let Some(lib) = sysroot() else {
        return;
    };
    let crt1 = std::fs::read(lib.join("crt1-reactor.o")).unwrap();
    let libc = std::fs::read(lib.join("libc.a")).unwrap();
    let options = Options {
        entry: None,
        exports: vec!["malloc".to_owned(), "free".to_owned()],
        ..Options::default()
    };
    let inputs =
        [Input { name: "crt1-reactor.o", bytes: &crt1 }, Input { name: "libc.a", bytes: &libc }];
    let module = link(&options, &inputs).unwrap_or_else(|e| panic!("{e}"));
    assert!(module.starts_with(b"\0asm\x01\0\0\0"));
    for export in ["_initialize", "malloc", "free"] {
        assert!(module.windows(export.len()).any(|w| w == export.as_bytes()), "{export}");
    }
    assert!(module.len() < 64 * 1024, "{}", module.len());
}
