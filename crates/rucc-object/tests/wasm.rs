//! The wasm object writer, checked two ways.
//!
//! The first reads the object back with a small reader in this file and checks that every
//! relocation entry points at a padded field that holds the value the entry implies. The second
//! links the object with `wasm-ld` against the wasi-sdk 34 sysroot and runs it under Wasmtime.
//! It needs `WASI_SDK_PATH`, which is the variable wasi-sdk's own build files read, and it says
//! that it did nothing when the variable is not set. When `wasm-tools` is on `PATH`, it also
//! validates the object and the module.

use std::path::{Path, PathBuf};
use std::process::Command;

use rucc_object::wasm::{
    self, Fixup, FuncType, Function, HIDDEN, LOCAL, Module, NO_STRIP, Place, Producers, RelocKind,
    STRINGS, Segment, SymbolKind, ValType,
};

/// `static int greet(const char *s) { return puts(s); }`, a pointer to it and a pointer to a
/// string in data, and `int main(void) { return fp(msg) < 0; }`, which calls through both. A
/// `main` with no parameters has the symbol `__main_void` on wasm, which is the name the start
/// code of wasi-libc calls.
fn program() -> Module {
    let mut m = Module::default();
    let one = m.intern(FuncType { params: vec![ValType::I32], results: vec![ValType::I32] });
    let none = m.intern(FuncType { params: vec![], results: vec![ValType::I32] });

    let main = m.symbol("__main_void", SymbolKind::Function { ty: none, import: None }, HIDDEN);
    let greet = m.symbol("greet", SymbolKind::Function { ty: one, import: None }, LOCAL);
    let puts = m.symbol("puts", SymbolKind::Function { ty: one, import: None }, 0);
    let text = b"hi from rucc\0";
    m.segments.push(Segment {
        name: ".rodata..L.str".into(),
        align: 0,
        flags: STRINGS,
        bytes: text.to_vec(),
        fixups: vec![],
    });
    let size = u32::try_from(text.len()).unwrap();
    let place = Some(Place { segment: 0, offset: 0, size });
    let string = m.symbol(".L.str", SymbolKind::Data { place }, LOCAL);
    let at = |s| Fixup { at: 0, kind: RelocKind::MemoryAddrI32, target: s, addend: 0 };
    m.segments.push(Segment {
        name: ".data.msg".into(),
        align: 2,
        flags: 0,
        bytes: vec![0; 4],
        fixups: vec![at(string)],
    });
    let place = Some(Place { segment: 1, offset: 0, size: 4 });
    let msg = m.symbol("msg", SymbolKind::Data { place }, HIDDEN);
    m.segments.push(Segment {
        name: ".data.fp".into(),
        align: 2,
        flags: 0,
        bytes: vec![0; 4],
        fixups: vec![Fixup { kind: RelocKind::TableIndexI32, ..at(greet) }],
    });
    let place = Some(Place { segment: 2, offset: 0, size: 4 });
    let fp = m.symbol("fp", SymbolKind::Data { place }, HIDDEN);
    let table = m.symbol("__indirect_function_table", SymbolKind::Table { import: None }, NO_STRIP);

    // local.get 0, call puts, end.
    let mut code = vec![0x20, 0x00, 0x10];
    code.extend_from_slice(&[0; 5]);
    code.push(0x0b);
    let call = Fixup { at: 3, kind: RelocKind::FunctionIndexLeb, target: puts, addend: 0 };
    let body = Function { symbol: greet, code, fixups: vec![call], ..Function::default() };
    m.functions.push(body);

    // i32.const 0, i32.load offset=msg, i32.const 0, i32.load offset=fp,
    // call_indirect (type one) (table), i32.const 0, i32.lt_s, end.
    let mut code = Vec::new();
    let mut fixups = Vec::new();
    let mut field = |code: &mut Vec<u8>, kind, target| {
        let at = u32::try_from(code.len()).unwrap();
        fixups.push(Fixup { at, kind, target, addend: 0 });
        code.extend_from_slice(&[0; 5]);
    };
    for symbol in [msg, fp] {
        code.extend_from_slice(&[0x41, 0x00, 0x28, 0x02]);
        field(&mut code, RelocKind::MemoryAddrLeb, symbol);
    }
    code.push(0x11);
    field(&mut code, RelocKind::TypeIndexLeb, one);
    field(&mut code, RelocKind::TableNumberLeb, table);
    code.extend_from_slice(&[0x41, 0x00, 0x48, 0x0b]);
    m.functions.push(Function { symbol: main, code, fixups, ..Function::default() });

    m.features = vec!["call-indirect-overlong".into()];
    m.disallowed = vec!["shared-mem".into()];
    m.producers = Producers {
        language: Some("C23".into()),
        processed_by: vec![("rucc".into(), env!("CARGO_PKG_VERSION").into())],
    };
    m
}

/// Read one unsigned LEB128 at `at` and say where it ended.
fn read_uleb(bytes: &[u8], at: &mut usize) -> u64 {
    let (mut value, mut shift) = (0u64, 0);
    loop {
        let byte = bytes[*at];
        *at += 1;
        value |= u64::from(byte & 0x7f) << shift;
        shift += 7;
        if byte & 0x80 == 0 {
            return value;
        }
    }
}

/// Read one signed LEB128 at `at`.
fn read_sleb(bytes: &[u8], at: &mut usize) -> i64 {
    let (mut value, mut shift) = (0i64, 0);
    loop {
        let byte = bytes[*at];
        *at += 1;
        value |= i64::from(byte & 0x7f) << shift;
        shift += 7;
        if byte & 0x80 == 0 {
            if shift < 64 && byte & 0x40 != 0 {
                value |= -1 << shift;
            }
            return value;
        }
    }
}

/// The sections of a module: the id, the name of a custom section, and the payload after it.
fn sections(bytes: &[u8]) -> Vec<(u8, String, Vec<u8>)> {
    assert_eq!(&bytes[..8], b"\0asm\x01\0\0\0");
    let mut at = 8;
    let mut out = Vec::new();
    while at < bytes.len() {
        let id = bytes[at];
        at += 1;
        let size = usize::try_from(read_uleb(bytes, &mut at)).unwrap();
        let mut payload = &bytes[at..at + size];
        at += size;
        let mut label = String::new();
        if id == 0 {
            let mut p = 0;
            let n = usize::try_from(read_uleb(payload, &mut p)).unwrap();
            label = String::from_utf8(payload[p..p + n].to_vec()).unwrap();
            payload = &payload[p + n..];
        }
        out.push((id, label, payload.to_vec()));
    }
    out
}

#[test]
fn the_sections_come_in_the_order_the_specification_and_lld_want() {
    let written = wasm::write(&program()).unwrap();
    let order: Vec<(u8, String)> =
        sections(&written.bytes).into_iter().map(|(id, label, _)| (id, label)).collect();
    let want = [
        (1, ""),
        (2, ""),
        (3, ""),
        (10, ""),
        (11, ""),
        (0, "linking"),
        (0, "reloc.CODE"),
        (0, "reloc.DATA"),
        (0, "producers"),
        (0, "target_features"),
    ];
    let want: Vec<(u8, String)> = want.iter().map(|(id, l)| (*id, (*l).to_owned())).collect();
    assert_eq!(order, want);
    assert_eq!(written.defines, ["__main_void", "msg", "fp"]);
}

#[test]
fn every_relocation_points_at_a_padded_field_that_holds_its_value() {
    let written = wasm::write(&program()).unwrap();
    let all = sections(&written.bytes);
    let mut seen = 0;
    for (_, label, payload) in all.iter().filter(|(_, l, _)| l.starts_with("reloc.")) {
        let mut at = 0;
        let target = usize::try_from(read_uleb(payload, &mut at)).unwrap();
        let patched = &all[target].2;
        let count = read_uleb(payload, &mut at);
        for _ in 0..count {
            let kind = payload[at];
            at += 1;
            let offset = usize::try_from(read_uleb(payload, &mut at)).unwrap();
            let symbol = read_uleb(payload, &mut at);
            if matches!(kind, 3..=5) {
                assert_eq!(read_sleb(payload, &mut at), 0);
            }
            let value = match kind {
                2 | 5 => {
                    i64::from(u32::from_le_bytes(patched[offset..offset + 4].try_into().unwrap()))
                }
                _ => {
                    let field = &patched[offset..offset + 5];
                    assert!(field[..4].iter().all(|b| b & 0x80 != 0), "{label}: {field:x?}");
                    let mut p = offset;
                    if kind == 4 {
                        read_sleb(patched, &mut p)
                    } else {
                        read_uleb(patched, &mut p) as i64
                    }
                }
            };
            // The function index of puts is 0, the only import, and greet's table slot is 1.
            // The string is at 0, msg at 16 after the 13 bytes of the string are aligned, and
            // fp at 20. The type of call_indirect is 0 and the table is number 0.
            let want = match (kind, symbol) {
                (0, 2) => 0,
                (2, 1) => 1,
                (3, 4) => 16,
                (3, 5) => 20,
                (5, 3) => 0,
                (6, 0) | (20, 6) => 0,
                other => panic!("{label}: an entry nobody wrote: {other:?}"),
            };
            assert_eq!(value, want, "{label}: kind {kind} symbol {symbol}");
            seen += 1;
        }
    }
    assert_eq!(seen, 7);
}

#[test]
fn a_fixup_that_names_the_wrong_kind_of_symbol_is_an_error() {
    let mut m = program();
    m.functions[0].fixups[0].kind = RelocKind::GlobalIndexLeb;
    assert!(matches!(wasm::write(&m), Err(wasm::Error::Target { .. })));
    let mut m = program();
    m.functions[0].fixups[0].at = 100;
    assert!(matches!(wasm::write(&m), Err(wasm::Error::Outside { .. })));
    let mut m = program();
    m.functions.push(m.functions[0].clone());
    assert!(matches!(wasm::write(&m), Err(wasm::Error::Definition { .. })));
}

/// A second name of a function is defined when the function that it names is defined in the
/// object, and it is an error when it names an import or data, or when it is listed twice.
#[test]
fn an_alias_is_defined_only_by_a_function_that_the_object_defines() {
    let index = |m: &Module, name: &str| {
        u32::try_from(m.symbols.iter().position(|s| s.name == name).unwrap()).unwrap()
    };
    let with_alias = |target: &str, twice: bool| {
        let mut m = program();
        let ty = m.intern(FuncType { params: vec![ValType::I32], results: vec![ValType::I32] });
        let hello = m.symbol("hello", SymbolKind::Function { ty, import: None }, HIDDEN);
        let target = index(&m, target);
        m.aliases.push((hello, target));
        if twice {
            m.aliases.push((hello, target));
        }
        wasm::write(&m)
    };
    let written = with_alias("greet", false).unwrap();
    assert!(written.defines.iter().any(|name| name == "hello"));
    for (target, twice) in [("puts", false), ("msg", false), ("greet", true)] {
        let refused = matches!(with_alias(target, twice), Err(wasm::Error::Alias { .. }));
        assert!(refused, "{target}");
    }
}

#[test]
fn the_padded_forms_read_back_as_their_values() {
    for value in [0u32, 1, 127, 128, 0xffff_ffff] {
        let field = wasm::uleb_padded(value);
        assert_eq!(read_uleb(&field, &mut 0), u64::from(value));
    }
    for value in [0i32, -1, 63, 64, -65, i32::MIN, i32::MAX] {
        let field = wasm::sleb_padded(value);
        assert_eq!(read_sleb(&field, &mut 0), i64::from(value));
        let mut short = Vec::new();
        wasm::sleb(&mut short, i64::from(value));
        assert_eq!(read_sleb(&short, &mut 0), i64::from(value));
    }
}

/// Where wasi-sdk is, or nothing when this machine has none.
fn sdk() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("WASI_SDK_PATH")?);
    path.join("bin/wasm-ld").exists().then_some(path)
}

/// A program on `PATH`, or nothing.
fn on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(program)).find(|p| p.exists())
}

fn run(program: &Path, args: &[&std::ffi::OsStr]) -> std::process::Output {
    let out = Command::new(program).args(args).output().expect("the tool starts");
    assert!(
        out.status.success() || program.ends_with("wasmtime"),
        "{}: {}",
        program.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

#[test]
fn the_object_links_with_wasm_ld_and_runs() {
    let Some(sdk) = sdk() else {
        eprintln!("WASI_SDK_PATH is not set, so the object was not linked");
        return;
    };
    let dir = std::env::temp_dir().join(format!("rucc-object-wasm-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let object = dir.join("a.o");
    std::fs::write(&object, wasm::write(&program()).unwrap().bytes).unwrap();

    let dump = run(&sdk.join("bin/llvm-objdump"), &["-t".as_ref(), object.as_os_str()]);
    let dump = String::from_utf8_lossy(&dump.stdout);
    assert!(
        dump.contains(".hidden __main_void") && dump.contains("*UND*") && dump.contains("puts")
    );

    let lib = sdk.join("share/wasi-sysroot/lib/wasm32-wasip1");
    let module = dir.join("a.wasm");
    let crt = lib.join("crt1-command.o");
    let search = format!("-L{}", lib.display());
    let args: Vec<&std::ffi::OsStr> = vec![
        "-m".as_ref(),
        "wasm32".as_ref(),
        search.as_ref(),
        crt.as_os_str(),
        object.as_os_str(),
        "-lc".as_ref(),
        "-o".as_ref(),
        module.as_os_str(),
    ];
    run(&sdk.join("bin/wasm-ld"), &args);

    if let Some(tools) = on_path("wasm-tools") {
        run(&tools, &["validate".as_ref(), object.as_os_str()]);
        run(&tools, &["validate".as_ref(), module.as_os_str()]);
    }
    let Some(wasmtime) = on_path("wasmtime") else {
        eprintln!("there is no wasmtime on PATH, so the module was not run");
        return;
    };
    let out = run(&wasmtime, &[module.as_os_str()]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hi from rucc\n");
    assert_eq!(out.status.code(), Some(0));
    std::fs::remove_dir_all(&dir).unwrap();
}
