//! The clang attributes `export_name`, `import_module` and `import_name` on the wasm rows, and
//! what gcc says about them on the other rows. The facts come from clang 23 from wasi-sdk 34: the
//! object has an export entry and the symbol is `EXPORTED` and `NO_STRIP`, an import has the
//! module and the field that the attributes give, and each refusal is an error in clang's words.
//!
//! Design: #2864, and section 11.5 of the WebAssembly notes.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

/// The output of rucc for `source` on stdin, with the arguments after the input.
fn rucc(target: &str, args: &[&str], source: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .env("LC_ALL", "C")
        .arg(format!("--target={target}"))
        .args(["-x", "c", "-"])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the compiler is built before its own tests run");
    child.stdin.take().expect("a pipe for the input").write_all(source.as_bytes()).unwrap();
    child.wait_with_output().expect("the compiler finished")
}

/// The object that rucc writes for `source` on `wasm32-wasip1`.
fn object(name: &str, source: &str) -> Vec<u8> {
    let path: PathBuf =
        std::env::temp_dir().join(format!("rucc-wasm-names-{}-{name}.o", std::process::id()));
    let out = rucc("wasm32-wasip1", &["-c", "-o", path.to_str().unwrap()], source);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let bytes = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    bytes
}

fn uleb(bytes: &[u8], at: &mut usize) -> u32 {
    let (mut value, mut shift) = (0u32, 0);
    loop {
        let byte = bytes[*at];
        *at += 1;
        value |= u32::from(byte & 0x7f) << shift;
        shift += 7;
        if byte < 0x80 {
            return value;
        }
    }
}

fn name(bytes: &[u8], at: &mut usize) -> String {
    let len = uleb(bytes, at) as usize;
    let text = String::from_utf8(bytes[*at..*at + len].to_vec()).unwrap();
    *at += len;
    text
}

/// The payload of each section of a module, with the id. A custom section is its name and the
/// rest of the payload.
fn sections(bytes: &[u8]) -> Vec<(u8, String, Vec<u8>)> {
    assert_eq!(&bytes[..8], b"\0asm\x01\0\0\0");
    let (mut at, mut out) = (8, Vec::new());
    while at < bytes.len() {
        let id = bytes[at];
        at += 1;
        let len = uleb(bytes, &mut at) as usize;
        let end = at + len;
        let mut inner = at;
        let custom = if id == 0 { name(bytes, &mut inner) } else { String::new() };
        out.push((id, custom, bytes[inner..end].to_vec()));
        at = end;
    }
    out
}

/// The name of each export of an object, which is a function export.
fn exports(bytes: &[u8]) -> Vec<String> {
    let Some((_, _, payload)) = sections(bytes).into_iter().find(|s| s.0 == 7) else {
        return Vec::new();
    };
    let mut at = 0;
    let count = uleb(&payload, &mut at);
    (0..count)
        .map(|_| {
            let export = name(&payload, &mut at);
            assert_eq!(payload[at], 0, "{export} is not a function");
            at += 1;
            uleb(&payload, &mut at);
            export
        })
        .collect()
}

/// The module and the field of each function import of an object.
fn imports(bytes: &[u8]) -> Vec<(String, String)> {
    let Some((_, _, payload)) = sections(bytes).into_iter().find(|s| s.0 == 2) else {
        return Vec::new();
    };
    let mut at = 0;
    let count = uleb(&payload, &mut at);
    let mut out = Vec::new();
    for _ in 0..count {
        let module = name(&payload, &mut at);
        let field = name(&payload, &mut at);
        let kind = payload[at];
        at += 1;
        match kind {
            0 => {
                uleb(&payload, &mut at);
                out.push((module, field));
            }
            1 => {
                at += 1;
                let flags = uleb(&payload, &mut at);
                uleb(&payload, &mut at);
                if flags & 1 != 0 {
                    uleb(&payload, &mut at);
                }
            }
            2 => {
                let flags = uleb(&payload, &mut at);
                uleb(&payload, &mut at);
                if flags & 1 != 0 {
                    uleb(&payload, &mut at);
                }
            }
            3 => at += 2,
            other => panic!("an import of kind {other}"),
        }
    }
    out
}

/// The flags of each function symbol of an object that is defined, by name.
fn defined_flags(bytes: &[u8]) -> Vec<(String, u32)> {
    let (_, _, payload) =
        sections(bytes).into_iter().find(|s| s.1 == "linking").expect("a linking section");
    let mut at = 0;
    assert_eq!(uleb(&payload, &mut at), 2);
    let mut out = Vec::new();
    while at < payload.len() {
        let kind = payload[at];
        at += 1;
        let len = uleb(&payload, &mut at) as usize;
        let end = at + len;
        if kind == 8 {
            let count = uleb(&payload, &mut at);
            for _ in 0..count {
                let symbol = payload[at];
                at += 1;
                let flags = uleb(&payload, &mut at);
                let undefined = flags & 0x10 != 0;
                match symbol {
                    0 | 2 | 4 | 5 => {
                        uleb(&payload, &mut at);
                        if !undefined || flags & 0x40 != 0 {
                            let named = name(&payload, &mut at);
                            if symbol == 0 && !undefined {
                                out.push((named, flags));
                            }
                        }
                    }
                    1 => {
                        name(&payload, &mut at);
                        if !undefined {
                            for _ in 0..3 {
                                uleb(&payload, &mut at);
                            }
                        }
                    }
                    _ => {
                        uleb(&payload, &mut at);
                    }
                }
            }
        }
        at = end;
    }
    out
}

const EXPORTED: u32 = 0x20;
const NO_STRIP: u32 = 0x80;

#[test]
fn an_export_name_is_an_export_of_the_object() {
    let source = "\
__attribute__((export_name(\"add\"))) int add(int a, int b) { return a + b; }
__attribute__((export_name(\"hid\"))) static int hidden(int a) { return a; }
int plain(void) { return 1; }
";
    let bytes = object("export", source);
    assert_eq!(exports(&bytes), ["add", "hid"]);
    let flags = defined_flags(&bytes);
    let of = |wanted: &str| flags.iter().find(|(n, _)| n == wanted).map(|&(_, f)| f);
    // `HIDDEN` and `LOCAL` stay as they are, and the export adds `EXPORTED` and `NO_STRIP`. The
    // static function is kept although nothing calls it, as `used` would keep it.
    assert_eq!(of("add"), Some(0x04 | EXPORTED | NO_STRIP), "{flags:?}");
    assert_eq!(of("hidden"), Some(0x02 | EXPORTED | NO_STRIP), "{flags:?}");
    assert_eq!(of("plain"), Some(0x04), "{flags:?}");
}

#[test]
fn an_import_module_and_an_import_name_are_the_import_of_the_object() {
    let source = "\
__attribute__((import_module(\"host\"), import_name(\"get\"))) int get(void);
__attribute__((import_name(\"put_it\"))) void put(int);
__attribute__((import_module(\"host\"))) int peek(void);
int other(void);
int use(void) { put(get()); return peek() + other(); }
";
    let bytes = object("import", source);
    let imports = imports(&bytes);
    for (module, field) in [("host", "get"), ("env", "put_it"), ("host", "peek"), ("env", "other")]
    {
        assert!(
            imports.iter().any(|(m, f)| m == module && f == field),
            "no import {module}.{field} in {imports:?}"
        );
    }
}

#[test]
fn the_wasm_names_are_checked_in_clang_s_words() {
    let source = "\
__attribute__((export_name(\"v\"))) int var;
__attribute__((export_name)) int f1(void);
__attribute__((export_name(3))) int f2(void);
__attribute__((export_name(\"a\", \"b\"))) int f3(void);
__attribute__((import_module)) int f4(void);
__attribute__((import_name(L\"w\"))) int f5(void);
";
    let out = rucc("wasm32-wasip1", &["-fsyntax-only"], source);
    assert!(!out.status.success());
    let said = String::from_utf8_lossy(&out.stderr);
    let expected = [
        (1, 16, "error: 'export_name' attribute only applies to functions"),
        (2, 16, "error: 'export_name' attribute takes one argument"),
        (3, 28, "error: expected string literal as argument of 'export_name' attribute"),
        (4, 16, "error: 'export_name' attribute takes one argument"),
        (5, 16, "error: 'import_module' attribute takes one argument"),
        (6, 28, "error: expected string literal as argument of 'import_name' attribute"),
    ];
    for (line, column, what) in expected {
        let at = format!(":{line}:{column}: {what}");
        assert!(said.lines().any(|l| l.contains(&at)), "missing {at:?} in\n{said}");
    }
    assert_eq!(said.matches("error:").count(), expected.len(), "{said}");
}

#[test]
fn the_wasm_names_are_unknown_to_gcc_on_the_other_rows() {
    let source = "\
__attribute__((export_name(\"add\"))) int add(int a, int b) { return a + b; }
__attribute__((import_module(\"host\"), import_name(\"get\"))) int get(void);
";
    let out = rucc("x86_64-linux-gnu", &["-fsyntax-only"], source);
    assert!(out.status.success());
    let said = String::from_utf8_lossy(&out.stderr);
    for (line, column, named) in
        [(1, 16, "export_name"), (2, 16, "import_module"), (2, 39, "import_name")]
    {
        let at = format!(":{line}:{column}: warning: '{named}' attribute directive ignored");
        assert!(said.lines().any(|l| l.contains(&at)), "missing {at:?} in\n{said}");
    }
    assert_eq!(said.matches("warning:").count(), 3, "{said}");
}
