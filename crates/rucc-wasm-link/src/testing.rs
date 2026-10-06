//! Small objects and archives built by hand, for the tests.

use crate::bytes::{name, uleb};

pub(crate) fn section(out: &mut Vec<u8>, id: u8, payload: &[u8]) {
    out.push(id);
    uleb(out, payload.len() as u64);
    out.extend_from_slice(payload);
}

pub(crate) fn custom(out: &mut Vec<u8>, title: &str, body: &[u8]) {
    let mut payload = Vec::new();
    name(&mut payload, title);
    payload.extend_from_slice(body);
    section(out, 0, &payload);
}

fn linking(out: &mut Vec<u8>, symbols: &[u8], segments: &[u8]) {
    let mut linking = vec![2, 8];
    uleb(&mut linking, symbols.len() as u64);
    linking.extend(symbols);
    if !segments.is_empty() {
        linking.push(5);
        uleb(&mut linking, segments.len() as u64);
        linking.extend(segments);
    }
    custom(out, "linking", &linking);
}

/// An object with one function, `caller` by name, that calls the undefined function `f` of type
/// `(i32) -> (i32)` with the address of `d`, the 4 bytes `abcd` in the segment `.data.d`.
pub(crate) fn caller(caller: &str) -> Vec<u8> {
    let mut out = b"\0asm\x01\0\0\0".to_vec();
    section(&mut out, 1, &[2, 0x60, 0, 0, 0x60, 1, 0x7f, 1, 0x7f]);
    let mut imports = vec![2];
    name(&mut imports, "env");
    name(&mut imports, "__linear_memory");
    imports.extend([2, 0, 1]);
    name(&mut imports, "env");
    name(&mut imports, "f");
    imports.extend([0, 1]);
    section(&mut out, 2, &imports);
    section(&mut out, 3, &[1, 0]);
    let body = [0, 0x41, 0x80, 0x80, 0x80, 0x80, 0, 0x10, 0x80, 0x80, 0x80, 0x80, 0, 0x1a, 0x0b];
    let mut code = vec![1, body.len() as u8];
    code.extend(body);
    section(&mut out, 10, &code);
    section(&mut out, 11, &[1, 0, 0x41, 0, 0x0b, 4, b'a', b'b', b'c', b'd']);
    let mut symbols = vec![3, 0, 0, 1];
    name(&mut symbols, caller);
    symbols.extend([0, 0x10, 0, 1, 0]);
    name(&mut symbols, "d");
    symbols.extend([0, 0, 4]);
    let mut info = vec![1];
    name(&mut info, ".data.d");
    info.extend([2, 0]);
    linking(&mut out, &symbols, &info);
    custom(&mut out, "reloc.CODE", &[3, 2, 4, 4, 2, 0, 0, 10, 1]);
    custom(
        &mut out,
        "target_features",
        &[1, b'+', 8, b's', b'i', b'g', b'n', b'-', b'e', b'x', b't'],
    );
    out
}

/// An object that defines the function `callee` with the type `ty`, as encoded, and the body
/// `unreachable`.
pub(crate) fn callee(callee: &str, ty: &[u8]) -> Vec<u8> {
    let mut out = b"\0asm\x01\0\0\0".to_vec();
    let mut types = vec![1];
    types.extend(ty);
    section(&mut out, 1, &types);
    section(&mut out, 3, &[1, 0]);
    section(&mut out, 10, &[1, 3, 0, 0x00, 0x0b]);
    let mut symbols = vec![1, 0, 0, 0];
    name(&mut symbols, callee);
    linking(&mut out, &symbols, &[]);
    out
}

/// A System V archive of the members.
pub(crate) fn archive(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = b"!<arch>\n".to_vec();
    for (name, bytes) in members {
        let header = format!(
            "{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
            format!("{name}/"),
            0,
            0,
            0,
            644,
            bytes.len()
        );
        out.extend(header.as_bytes());
        out.extend(*bytes);
        if bytes.len() % 2 == 1 {
            out.push(b'\n');
        }
    }
    out
}

/// An object with the function `_start`, which calls the functions `a`, `b` and `c` of the
/// module `m`, in that order.
pub(crate) fn importer() -> Vec<u8> {
    let mut out = b"\0asm\x01\0\0\0".to_vec();
    section(&mut out, 1, &[1, 0x60, 0, 0]);
    let mut imports = vec![3];
    for field in ["a", "b", "c"] {
        name(&mut imports, "m");
        name(&mut imports, field);
        imports.extend([0, 0]);
    }
    section(&mut out, 2, &imports);
    section(&mut out, 3, &[1, 0]);
    let mut body = vec![0];
    for _ in 0..3 {
        body.extend([0x10, 0x80, 0x80, 0x80, 0x80, 0]);
    }
    body.push(0x0b);
    let mut code = vec![1, body.len() as u8];
    code.extend(body);
    section(&mut out, 10, &code);
    let mut symbols = vec![4, 0, 0, 3];
    name(&mut symbols, "_start");
    symbols.extend([0, 0x10, 0, 0, 0x10, 1, 0, 0x10, 2]);
    linking(&mut out, &symbols, &[]);
    custom(&mut out, "reloc.CODE", &[3, 3, 0, 4, 1, 0, 10, 2, 0, 16, 3]);
    out
}

/// An object that defines the function `name` of the type `() -> ()`, which calls the undefined
/// function `callee` of the same type, when there is one, and else does nothing.
pub(crate) fn calls(name_: &str, callee: Option<&str>) -> Vec<u8> {
    let mut out = b"\0asm\x01\0\0\0".to_vec();
    section(&mut out, 1, &[1, 0x60, 0, 0]);
    let mut symbols = Vec::new();
    let mut body = vec![0];
    if let Some(callee) = callee {
        let mut imports = vec![1];
        name(&mut imports, "env");
        name(&mut imports, callee);
        imports.extend([0, 0]);
        section(&mut out, 2, &imports);
        symbols.extend([2, 0, 0x10, 0, 0, 0, 1]);
        body.extend([0x10, 0x80, 0x80, 0x80, 0x80, 0]);
    } else {
        symbols.extend([1, 0, 0, 0]);
    }
    name(&mut symbols, name_);
    body.push(0x0b);
    section(&mut out, 3, &[1, 0]);
    let mut code = vec![1, body.len() as u8];
    code.extend(body);
    section(&mut out, 10, &code);
    linking(&mut out, &symbols, &[]);
    if callee.is_some() {
        custom(&mut out, "reloc.CODE", &[3, 1, 0, 4, 0]);
    }
    out
}

/// An object with the function `_start`, which calls the two undefined weak functions `callees`,
/// both of the type `() -> ()`, in that order.
pub(crate) fn weak_caller(callees: [&str; 2]) -> Vec<u8> {
    let mut out = b"\0asm\x01\0\0\0".to_vec();
    section(&mut out, 1, &[1, 0x60, 0, 0]);
    let mut imports = vec![2];
    for field in callees {
        name(&mut imports, "env");
        name(&mut imports, field);
        imports.extend([0, 0]);
    }
    section(&mut out, 2, &imports);
    section(&mut out, 3, &[1, 0]);
    let body = [0, 0x10, 0x80, 0x80, 0x80, 0x80, 0, 0x10, 0x80, 0x80, 0x80, 0x80, 0, 0x0b];
    let mut code = vec![1, body.len() as u8];
    code.extend(body);
    section(&mut out, 10, &code);
    let mut symbols = vec![3, 0, 0x11, 0, 0, 0x11, 1, 0, 0, 2];
    name(&mut symbols, "_start");
    linking(&mut out, &symbols, &[]);
    custom(&mut out, "reloc.CODE", &[3, 2, 0, 4, 0, 0, 10, 1]);
    out
}

/// An object that defines the function `name` of the type `() -> ()`, with DWARF sections as
/// LLVM writes them. `.debug_str` holds `name` and `shared`, each with its NUL. `.debug_info`
/// holds the address of the second byte of the function, which follows its symbol, and the offset
/// of `shared` in `.debug_str`, which follows a section symbol.
pub(crate) fn debugged(name_: &str) -> Vec<u8> {
    let mut out = b"\0asm\x01\0\0\0".to_vec();
    section(&mut out, 1, &[1, 0x60, 0, 0]);
    section(&mut out, 3, &[1, 0]);
    section(&mut out, 10, &[1, 2, 0, 0x0b]);
    custom(&mut out, ".debug_str", format!("{name_}\0shared\0").as_bytes());
    custom(&mut out, ".debug_info", &[0; 8]);
    let mut symbols = vec![2, 0, 0, 0];
    name(&mut symbols, name_);
    // The section symbol of section 3, `.debug_str`, which is local.
    symbols.extend([3, 0x2, 3]);
    linking(&mut out, &symbols, &[]);
    let shared = name_.len() as u8 + 1;
    custom(&mut out, "reloc..debug_info", &[4, 2, 8, 0, 0, 1, 9, 4, 1, shared]);
    out
}
