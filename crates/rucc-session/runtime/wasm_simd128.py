#!/usr/bin/env python3
"""Writes crates/rucc-session/runtime/include/wasm_simd128.h. Run from the repository root."""

import re
import sys

# name, C lane type, bits, kind (s signed, u unsigned, f float), the lane type of the interface
ELEMS = [
    ("i8", "signed char", 8, "s", "int8_t"),
    ("u8", "unsigned char", 8, "u", "uint8_t"),
    ("i16", "short", 16, "s", "int16_t"),
    ("u16", "unsigned short", 16, "u", "uint16_t"),
    ("i32", "int", 32, "s", "int32_t"),
    ("u32", "unsigned int", 32, "u", "uint32_t"),
    ("i64", "long long", 64, "s", "int64_t"),
    ("u64", "unsigned long long", 64, "u", "uint64_t"),
    ("f32", "float", 32, "f", "float"),
    ("f64", "double", 64, "f", "double"),
]
BY = {e[0]: e for e in ELEMS}

out = []
w = out.append


def lanes(e):
    return 128 // BY[e][2]


def vt(e):
    """The vector of 16 bytes with lanes of e, as `<wasm_simd128.h>` of clang names it."""
    return f"__{e}x{lanes(e)}"


def ct(e):
    """The lane type of e in the interface, which is what make, splat and extract_lane take."""
    return BY[e][4]


def ut(e):
    """The unsigned lane of the same width, where an add, a subtract or a multiply wraps."""
    return "u" + e[1:]


def st(e):
    return "i" + e[1:]


def wide(e):
    return e[0] + str(BY[e][2] * 2)


def narrow(e):
    return e[0] + str(BY[e][2] // 2)


def vname(e):
    """The prefix of a name, which is the lane type and the number of lanes."""
    return f"wasm_{e}x{lanes(e)}"


def bounds(e):
    bits, kind = BY[e][2], BY[e][3]
    if kind == "u":
        return 0, (1 << bits) - 1
    return -(1 << (bits - 1)), (1 << (bits - 1)) - 1


# The names a function uses for its parameters and locals. They are written short below and made
# reserved here, so that a program with a macro called `a` or `mem` can still include the header.
LOCAL = re.compile(r"\b(a|b|c|m|x|y|z|r|s|i|v|mem|vec|c\d+)\b")


def reserve(text):
    return LOCAL.sub(r"__\1", text)


def statements(body):
    """Splits a body written on one line at each semicolon outside parentheses and braces."""
    out, depth, cur = [], 0, ""
    for ch in body:
        cur += ch
        depth += ch in "({"
        depth -= ch in ")}"
        if ch == ";" and depth == 0:
            out.append(cur.strip())
            cur = ""
    if cur.strip():
        out.append(cur.strip())
    return out


def wrap(stmt, indent):
    """Keeps a statement inside a hundred columns: a loop's body goes on the next line, and a long
    expression breaks after an operator."""
    if len(indent) + len(stmt) <= 100:
        return [indent + stmt]
    if stmt.startswith("for ("):
        depth = 0
        for at, ch in enumerate(stmt):
            depth += ch == "("
            depth -= ch == ")"
            if depth == 0 and ch == ")":
                break
        return [indent + stmt[: at + 1]] + wrap(stmt[at + 1 :].strip(), indent + "  ")
    out, first = [], True
    while len(indent) + len(stmt) + (0 if first else 4) > 100:
        room = 100 - len(indent) - (0 if first else 4)
        cut = max(stmt.rfind(op, 0, room) for op in (" ? ", " : ", " | ", " + ", ", ", " = "))
        if cut <= 0:
            break
        out.append(indent + ("" if first else "    ") + stmt[: cut + 1].rstrip())
        stmt, first = stmt[cut + 1 :].lstrip(), False
    out.append(indent + ("" if first else "    ") + stmt)
    return out


NAMES = []


def fill(text, indent):
    """Breaks text after a comma so that no line passes a hundred columns, with the lines after
    the first indented by four."""
    lines, cur = [], ""
    for part in text.split(", "):
        piece = part if not cur else ", " + part
        if cur and len(cur) + len(piece) > 99:
            lines.append(cur + ",")
            cur, piece = indent + "    ", part
        cur += piece
    return lines + [cur]


def fn(ret, nm, params, body, attrs=""):
    NAMES.append(nm)
    if attrs:
        w(f"static __inline__ {attrs}")
        head = f"{ret} {nm}({reserve(params)}) {{"
    else:
        head = f"static __inline__ {ret} {nm}({reserve(params)}) {{"
    for line in fill(head, ""):
        w(line)
    for stmt in body if isinstance(body, list) else statements(body):
        for line in wrap(reserve(stmt), "  "):
            w(line)
    w("}")


def lanewise(ret_e, nm, params, setup, expr):
    """A function whose answer is a vector of ret_e made one lane at a time. setup is the
    statements that name the operands as vectors of the lane types the expression reads."""
    rt = vt(ret_e)
    fn(
        "v128_t",
        nm,
        params,
        f"{setup} {rt} r; for (int i = 0; i < {lanes(ret_e)}; i++) r[i] = {expr}; "
        "return (v128_t)r;",
    )


def one(e, name="x", of="a"):
    return f"{vt(e)} {name} = ({vt(e)}){of};"


def two(e, f=None):
    f = f or e
    return f"{one(e)} {one(f, 'y', 'b')}"


SIGNED = ["i8", "i16", "i32", "i64"]
UNSIGNED = ["u8", "u16", "u32", "u64"]
INTS = SIGNED + UNSIGNED
FLOATS = ["f32", "f64"]


INTRO = """
/* wasm_simd128.h, the WebAssembly 128-bit SIMD intrinsics.
 *
 * Written by `crates/rucc-session/runtime/wasm_simd128.py`, which is the file to change. Run it
 * from the root of the repository and it writes this one.
 *
 * The names, the types and what each function computes are those of the `<wasm_simd128.h>` of
 * clang 23, for the instructions of the simd128 and relaxed-simd proposals. Every function is C
 * over the lanes of a GNU vector, the same way `<arm_neon.h>` is written, so what a program
 * computes is what the instruction computes and what it compiles to is one lane at a time until
 * rucc writes v128 instructions. For that reason the header does not need `-msimd128`, and a
 * module from it runs on an engine with no SIMD.
 *
 * Each relaxed function gives one of the answers that the relaxed-simd proposal allows, and the
 * same one on every engine: the multiply add is not fused, the lane select is a bit select, the
 * swizzle, the minimum, the maximum, the truncation and the rounding multiply are those of
 * simd128, and the dot products take the second operand as signed.
 *
 * The rounding of a float lane to an integer is exact C, with no call to the library. The square
 * root is `__builtin_sqrtf` and `__builtin_sqrt`, which call `sqrtf` and `sqrt`, so a program for
 * `wasm32-none` that uses a square root must supply them. The half precision functions of the
 * fp16 proposal are not here. tests/wasm-simd calls every function and macro here and holds each
 * one to the answers of clang's own header, for the relaxed functions over the inputs that have
 * one answer. */
"""


def split(text, indent):
    """Breaks text after a comma so that no line, with a backslash after it, passes a hundred
    columns."""
    lines, cur = [], indent
    for part in text.split(", "):
        piece = part if cur.strip() == "" else ", " + part
        if len(cur) + len(piece) > 97 and cur.strip():
            lines.append(cur + ",")
            cur, piece = indent + "  ", part
        cur += piece
    lines.append(cur)
    return lines


def macro(head, body):
    """A macro, with each line but the last ending in a backslash."""
    lines = split("#define " + head, "") + split(body, "  ")
    for line in lines[:-1]:
        w(line + " \\")
    w(lines[-1])


def section(title):
    w("")
    w(f"/* {title} */")


def emit():
    for line in INTRO.strip().splitlines():
        w(line)
    w("")
    w("#ifndef __RUCC_WASM_SIMD128_H")
    w("#define __RUCC_WASM_SIMD128_H")
    w("")
    w("#if !defined(__wasm__)")
    w('#error "wasm_simd128.h is for WebAssembly"')
    w("#endif")
    w("")
    w("#include <stdbool.h>")
    w("#include <stdint.h>")
    w("")
    w("typedef int32_t v128_t __attribute__((__vector_size__(16), __aligned__(16)));")
    w("typedef int32_t __v128_u __attribute__((__vector_size__(16), __aligned__(1)));")
    for e in INTS + FLOATS:
        w(f"typedef {BY[e][1]} {vt(e)} __attribute__((__vector_size__(16), __aligned__(16)));")
    for e in ("i8", "u8", "i16", "u16", "i32", "u32", "f32"):
        n = lanes(e) // 2
        w(f"typedef {BY[e][1]} __{e}x{n} __attribute__((__vector_size__(8), __aligned__(8)));")

    helpers = len(NAMES)

    section("Loads and stores. Through memcpy, since none of them has to be aligned.")
    fn(
        "v128_t",
        "wasm_v128_load",
        "const void *mem",
        "v128_t r; __builtin_memcpy(&r, mem, 16); return r;",
    )
    for bits, e in ((8, "u8"), (16, "u16"), (32, "u32"), (64, "u64")):
        c = ct(e)
        fn(
            "v128_t",
            f"wasm_v128_load{bits}_splat",
            "const void *mem",
            f"{c} v; __builtin_memcpy(&v, mem, sizeof v); "
            f"return (v128_t)({vt(e)}){{{', '.join(['v'] * lanes(e))}}};",
        )
    for e in ("i16", "u16", "i32", "u32", "i64", "u64"):
        n = narrow(e)
        k = lanes(e)
        fn(
            "v128_t",
            f"{vname(e)}_load{BY[n][2]}x{k}",
            "const void *mem",
            f"{ct(n)} v[{k}]; __builtin_memcpy(v, mem, sizeof v); {vt(e)} r; "
            f"for (int i = 0; i < {k}; i++) r[i] = v[i]; return (v128_t)r;",
        )
    for bits, e in ((32, "u32"), (64, "u64")):
        c = ct(e)
        zeros = ", 0" * (lanes(e) - 1)
        fn(
            "v128_t",
            f"wasm_v128_load{bits}_zero",
            "const void *mem",
            f"{c} v; __builtin_memcpy(&v, mem, sizeof v); return (v128_t)({vt(e)}){{v{zeros}}};",
        )
    for bits, e in ((8, "u8"), (16, "u16"), (32, "u32"), (64, "u64")):
        c = ct(e)
        fn(
            "v128_t",
            f"wasm_v128_load{bits}_lane",
            "const void *mem, v128_t vec, int i",
            f"{c} v; __builtin_memcpy(&v, mem, sizeof v); {one(e, 'r', 'vec')} r[i] = v; "
            "return (v128_t)r;",
        )
    fn("void", "wasm_v128_store", "void *mem, v128_t a", "__builtin_memcpy(mem, &a, 16);")
    for bits, e in ((8, "u8"), (16, "u16"), (32, "u32"), (64, "u64")):
        c = ct(e)
        fn(
            "void",
            f"wasm_v128_store{bits}_lane",
            "void *mem, v128_t vec, int i",
            f"{one(e, 'x', 'vec')} {c} v = x[i]; __builtin_memcpy(mem, &v, sizeof v);",
        )

    section("Making vectors and taking them apart. The const forms are the same as the others.")
    for e in INTS + FLOATS:
        n, c, v = lanes(e), ct(e), vt(e)
        cs = [f"c{i}" for i in range(n)]
        params = ", ".join(f"{c} {x}" for x in cs)
        fn("v128_t", f"{vname(e)}_make", params, f"return (v128_t)({v}){{{', '.join(cs)}}};")
    for e in INTS + FLOATS:
        n, c, v = lanes(e), ct(e), vt(e)
        cs = [f"c{i}" for i in range(n)]
        params = ", ".join(f"{c} {x}" for x in cs)
        fn("v128_t", f"{vname(e)}_const", params, f"return (v128_t)({v}){{{', '.join(cs)}}};")
    for e in INTS + FLOATS:
        n, c, v = lanes(e), ct(e), vt(e)
        fn(
            "v128_t",
            f"{vname(e)}_const_splat",
            f"{c} c",
            f"return (v128_t)({v}){{{', '.join(['c'] * n)}}};",
        )
    for e in INTS + FLOATS:
        n, c, v = lanes(e), ct(e), vt(e)
        fn(
            "v128_t",
            f"{vname(e)}_splat",
            f"{c} a",
            f"return (v128_t)({v}){{{', '.join(['a'] * n)}}};",
        )
    for e in INTS + FLOATS:
        fn(ct(e), f"{vname(e)}_extract_lane", "v128_t a, int i", f"{one(e)} return x[i];")
    for e in INTS + FLOATS:
        fn(
            "v128_t",
            f"{vname(e)}_replace_lane",
            f"v128_t a, int i, {ct(e)} b",
            f"{one(e, 'r')} r[i] = b; return (v128_t)r;",
        )

    section("Integer arithmetic. The lanes that wrap are unsigned, where C has them wrap too.")
    for e in SIGNED:
        u = ut(e)
        for op, sym in (("add", "+"), ("sub", "-")):
            fn(
                "v128_t",
                f"{vname(e)}_{op}",
                "v128_t a, v128_t b",
                f"return (v128_t)(({vt(u)})a {sym} ({vt(u)})b);",
            )
        if e != "i8":
            fn(
                "v128_t",
                f"{vname(e)}_mul",
                "v128_t a, v128_t b",
                f"return (v128_t)(({vt(u)})a * ({vt(u)})b);",
            )
        fn("v128_t", f"{vname(e)}_neg", "v128_t a", f"return (v128_t)(-({vt(u)})a);")
        lanewise(
            u, f"{vname(e)}_abs", "v128_t a", f"{one(e)} {one(u, 'y')}", "x[i] < 0 ? -y[i] : y[i]"
        )
        mask = BY[e][2] - 1
        fn(
            "v128_t",
            f"{vname(e)}_shl",
            "v128_t a, uint32_t b",
            f"return (v128_t)(({vt(u)})a << (b & {mask}));",
        )
        fn(
            "v128_t",
            f"{vname(e)}_shr",
            "v128_t a, uint32_t b",
            f"return (v128_t)(({vt(e)})a >> (b & {mask}));",
        )
        fn(
            "v128_t",
            f"{vname(u)}_shr",
            "v128_t a, uint32_t b",
            f"return (v128_t)(({vt(u)})a >> (b & {mask}));",
        )
        fn(
            "bool",
            f"{vname(e)}_all_true",
            "v128_t a",
            f"{one(e)} for (int i = 0; i < {lanes(e)}; i++) if (x[i] == 0) return false; "
            "return true;",
        )
        fn(
            "uint32_t",
            f"{vname(e)}_bitmask",
            "v128_t a",
            f"{one(e)} uint32_t m = 0; for (int i = 0; i < {lanes(e)}; i++) "
            "m |= (uint32_t)(x[i] < 0) << i; return m;",
        )
    lanewise("u8", "wasm_i8x16_popcnt", "v128_t a", one("u8"), "__builtin_popcount(x[i])")
    for e in ("i8", "u8", "i16", "u16"):
        lo, hi = bounds(e)
        for op, sym in (("add_sat", "+"), ("sub_sat", "-")):
            fn(
                "v128_t",
                f"{vname(e)}_{op}",
                "v128_t a, v128_t b",
                f"{two(e)} {vt(e)} r; for (int i = 0; i < {lanes(e)}; i++) "
                f"{{ int s = x[i] {sym} y[i]; r[i] = s < {lo} ? {lo} : s > {hi} ? {hi} : s; }} "
                "return (v128_t)r;",
            )
    for e in ("i8", "u8", "i16", "u16", "i32", "u32"):
        lanewise(e, f"{vname(e)}_min", "v128_t a, v128_t b", two(e), "x[i] < y[i] ? x[i] : y[i]")
        lanewise(e, f"{vname(e)}_max", "v128_t a, v128_t b", two(e), "x[i] < y[i] ? y[i] : x[i]")
    for e in ("u8", "u16"):
        lanewise(e, f"{vname(e)}_avgr", "v128_t a, v128_t b", two(e), "(x[i] + y[i] + 1) >> 1")
    lanewise(
        "i16",
        "wasm_i16x8_q15mulr_sat",
        "v128_t a, v128_t b",
        two("i16") + " int s;",
        "(s = (x[i] * y[i] + 0x4000) >> 15) > 32767 ? 32767 : s",
    )
    lanewise(
        "u32",
        "wasm_i32x4_dot_i16x8",
        "v128_t a, v128_t b",
        two("i16"),
        "(unsigned)(x[2 * i] * y[2 * i]) + (unsigned)(x[2 * i + 1] * y[2 * i + 1])",
    )

    section("Comparisons. A lane that holds is all ones and a lane that does not is zero.")
    for e in INTS + FLOATS:
        ops = ["eq", "ne", "lt", "gt", "le", "ge"]
        if e in UNSIGNED:
            ops = [] if e == "u64" else ops[2:]
        for op in ops:
            sym = {"eq": "==", "ne": "!=", "lt": "<", "gt": ">", "le": "<=", "ge": ">="}[op]
            fn(
                "v128_t",
                f"{vname(e)}_{op}",
                "v128_t a, v128_t b",
                f"return (v128_t)(({vt(e)})a {sym} ({vt(e)})b);",
            )

    section("Float arithmetic.")
    for e in FLOATS:
        v, u = vt(e), vt("u" + e[1:])
        sign = "0x80000000u" if e == "f32" else "0x8000000000000000ull"
        fn("v128_t", f"{vname(e)}_abs", "v128_t a", f"return (v128_t)(({u})a & ~{sign});")
        fn("v128_t", f"{vname(e)}_neg", "v128_t a", f"return (v128_t)(({u})a ^ {sign});")
        root = "__builtin_sqrtf" if e == "f32" else "__builtin_sqrt"
        lanewise(e, f"{vname(e)}_sqrt", "v128_t a", one(e), f"{root}(x[i])")
        # Each builtin is the wasm instruction of the same name, and `rint` is `nearest`.
        f = "f" if e == "f32" else ""
        rounding = {"ceil": "ceil", "floor": "floor", "trunc": "trunc", "nearest": "rint"}
        for op, libm in rounding.items():
            lanewise(e, f"{vname(e)}_{op}", "v128_t a", one(e), f"__builtin_{libm}{f}(x[i])")
        for op, sym in (("add", "+"), ("sub", "-"), ("mul", "*"), ("div", "/")):
            fn(
                "v128_t",
                f"{vname(e)}_{op}",
                "v128_t a, v128_t b",
                f"return (v128_t)(({v})a {sym} ({v})b);",
            )
        for op in ("min", "max"):
            lanewise(
                e,
                f"{vname(e)}_{op}",
                "v128_t a, v128_t b",
                two(e),
                f"__builtin_wasm_{op}_{e}(x[i], y[i])",
            )
        lanewise(e, f"{vname(e)}_pmin", "v128_t a, v128_t b", two(e), "y[i] < x[i] ? y[i] : x[i]")
        lanewise(e, f"{vname(e)}_pmax", "v128_t a, v128_t b", two(e), "x[i] < y[i] ? y[i] : x[i]")

    section("Conversions, narrowing and widening.")
    lanewise("f32", "wasm_f32x4_convert_i32x4", "v128_t a", one("i32"), "x[i]")
    lanewise("f32", "wasm_f32x4_convert_u32x4", "v128_t a", one("u32"), "x[i]")
    lanewise("f64", "wasm_f64x2_convert_low_i32x4", "v128_t a", one("i32"), "x[i]")
    lanewise("f64", "wasm_f64x2_convert_low_u32x4", "v128_t a", one("u32"), "x[i]")
    for e, sign in (("i32", "s"), ("u32", "u")):
        lanewise(
            e,
            f"{vname(e)}_trunc_sat_f32x4",
            "v128_t a",
            one("f32"),
            f"__builtin_wasm_trunc_saturate_{sign}_i32_f32(x[i])",
        )
        lanewise(
            e,
            f"{vname(e)}_trunc_sat_f64x2_zero",
            "v128_t a",
            one("f64"),
            f"i < 2 ? __builtin_wasm_trunc_saturate_{sign}_i32_f64(x[i]) : 0",
        )
    lanewise(
        "f32", "wasm_f32x4_demote_f64x2_zero", "v128_t a", one("f64"), "i < 2 ? (float)x[i] : 0"
    )
    lanewise("f64", "wasm_f64x2_promote_low_f32x4", "v128_t a", one("f32"), "x[i]")
    for e in ("i8", "u8", "i16", "u16"):
        src = wide(st(e))
        lo, hi = bounds(e)
        n = lanes(src)
        lanewise(
            e,
            f"{vname(e)}_narrow_{src}x{n}",
            "v128_t a, v128_t b",
            two(src, src) + " int s;",
            f"(s = i < {n} ? x[i] : y[i - {n}]) < {lo} ? {lo} : s > {hi} ? {hi} : s",
        )
    for e in ("i16", "u16", "i32", "u32", "i64", "u64"):
        n = narrow(e)
        k = lanes(e)
        lanewise(e, f"{vname(e)}_extend_low_{n}x{lanes(n)}", "v128_t a", one(n), "x[i]")
        lanewise(e, f"{vname(e)}_extend_high_{n}x{lanes(n)}", "v128_t a", one(n), f"x[i + {k}]")
    for e in ("i16", "u16", "i32", "u32", "i64", "u64"):
        n = narrow(e)
        k = lanes(e)
        c = BY[e][1]
        lanewise(
            e,
            f"{vname(e)}_extmul_low_{n}x{lanes(n)}",
            "v128_t a, v128_t b",
            two(n),
            f"({c})x[i] * ({c})y[i]",
        )
        lanewise(
            e,
            f"{vname(e)}_extmul_high_{n}x{lanes(n)}",
            "v128_t a, v128_t b",
            two(n),
            f"({c})x[i + {k}] * ({c})y[i + {k}]",
        )
    for e in ("i16", "u16", "i32", "u32"):
        n = narrow(e)
        lanewise(
            e,
            f"{vname(e)}_extadd_pairwise_{n}x{lanes(n)}",
            "v128_t a",
            one(n),
            "x[2 * i] + x[2 * i + 1]",
        )

    section("Bitwise operations, the any test and the swizzle.")
    fn("v128_t", "wasm_v128_not", "v128_t a", "return ~a;")
    fn("v128_t", "wasm_v128_and", "v128_t a, v128_t b", "return a & b;")
    fn("v128_t", "wasm_v128_or", "v128_t a, v128_t b", "return a | b;")
    fn("v128_t", "wasm_v128_xor", "v128_t a, v128_t b", "return a ^ b;")
    fn("v128_t", "wasm_v128_andnot", "v128_t a, v128_t b", "return a & ~b;")
    fn("bool", "wasm_v128_any_true", "v128_t a", f"{one('u64')} return (x[0] | x[1]) != 0;")
    fn(
        "v128_t",
        "wasm_v128_bitselect",
        "v128_t a, v128_t b, v128_t m",
        "return (a & m) | (b & ~m);",
    )
    lanewise("u8", "wasm_i8x16_swizzle", "v128_t a, v128_t b", two("u8"), "y[i] < 16 ? x[y[i]] : 0")

    section(
        "The relaxed-simd functions. Each gives one of the answers the proposal allows, and the "
        "same\n * one on every engine."
    )
    lanewise(
        "u8",
        "wasm_i8x16_relaxed_swizzle",
        "v128_t a, v128_t s",
        f"{one('u8')} {one('u8', 'y', 's')}",
        "y[i] < 16 ? x[y[i]] : 0",
    )
    for e in ("i32", "u32"):
        fn(
            "v128_t",
            f"{vname(e)}_relaxed_trunc_f32x4",
            "v128_t a",
            f"return {vname(e)}_trunc_sat_f32x4(a);",
        )
        fn(
            "v128_t",
            f"{vname(e)}_relaxed_trunc_f64x2_zero",
            "v128_t a",
            f"return {vname(e)}_trunc_sat_f64x2_zero(a);",
        )
    for e in FLOATS:
        v = vt(e)
        fn(
            "v128_t",
            f"{vname(e)}_relaxed_madd",
            "v128_t a, v128_t b, v128_t c",
            f"return (v128_t)(({v})a * ({v})b + ({v})c);",
        )
        fn(
            "v128_t",
            f"{vname(e)}_relaxed_nmadd",
            "v128_t a, v128_t b, v128_t c",
            f"return (v128_t)(-(({v})a * ({v})b) + ({v})c);",
        )
        for op in ("min", "max"):
            fn(
                "v128_t",
                f"{vname(e)}_relaxed_{op}",
                "v128_t a, v128_t b",
                f"return {vname(e)}_{op}(a, b);",
            )
    for e in SIGNED:
        fn(
            "v128_t",
            f"{vname(e)}_relaxed_laneselect",
            "v128_t a, v128_t b, v128_t m",
            "return wasm_v128_bitselect(a, b, m);",
        )
    fn(
        "v128_t",
        "wasm_i16x8_relaxed_q15mulr",
        "v128_t a, v128_t b",
        "return wasm_i16x8_q15mulr_sat(a, b);",
    )
    lanewise(
        "i16",
        "wasm_i16x8_relaxed_dot_i8x16_i7x16",
        "v128_t a, v128_t b",
        two("i8"),
        "x[2 * i] * y[2 * i] + x[2 * i + 1] * y[2 * i + 1]",
    )
    fn(
        "v128_t",
        "wasm_i32x4_relaxed_dot_i8x16_i7x16_add",
        "v128_t a, v128_t b, v128_t c",
        f"{two('i8')} {one('u32', 'z', 'c')} {vt('u32')} r; for (int i = 0; i < 4; i++) "
        "r[i] = (unsigned)(x[4 * i] * y[4 * i] + x[4 * i + 1] * y[4 * i + 1] + "
        "x[4 * i + 2] * y[4 * i + 2] + x[4 * i + 3] * y[4 * i + 3]) + z[i]; return (v128_t)r;",
    )

    section(
        "The shuffles, which pick each lane of the answer from the lanes of both operands. A "
        "lane index\n * past the end of both picks from the start again, as `__builtin_shuffle` "
        "does."
    )
    for e in SIGNED:
        n = lanes(e)
        u = vt(ut(e))
        cs = ", ".join(f"__c{i}" for i in range(n))
        macro(
            f"{vname(e)}_shuffle(__a, __b, {cs})",
            f"((v128_t)__builtin_shuffle(({u})(__a), ({u})(__b), ({u}){{{cs}}}))",
        )

    section("The names of earlier versions of the proposal, which clang keeps as deprecated.")
    old = [
        ("wasm_v8x16_load_splat", "wasm_v128_load8_splat", "const void *mem", "mem"),
        ("wasm_v16x8_load_splat", "wasm_v128_load16_splat", "const void *mem", "mem"),
        ("wasm_v32x4_load_splat", "wasm_v128_load32_splat", "const void *mem", "mem"),
        ("wasm_v64x2_load_splat", "wasm_v128_load64_splat", "const void *mem", "mem"),
        ("wasm_i16x8_load_8x8", "wasm_i16x8_load8x8", "const void *mem", "mem"),
        ("wasm_u16x8_load_8x8", "wasm_u16x8_load8x8", "const void *mem", "mem"),
        ("wasm_i32x4_load_16x4", "wasm_i32x4_load16x4", "const void *mem", "mem"),
        ("wasm_u32x4_load_16x4", "wasm_u32x4_load16x4", "const void *mem", "mem"),
        ("wasm_i64x2_load_32x2", "wasm_i64x2_load32x2", "const void *mem", "mem"),
        ("wasm_u64x2_load_32x2", "wasm_u64x2_load32x2", "const void *mem", "mem"),
        ("wasm_v8x16_swizzle", "wasm_i8x16_swizzle", "v128_t a, v128_t b", "a, b"),
        ("wasm_i8x16_any_true", "wasm_v128_any_true", "v128_t a", "a"),
        ("wasm_i16x8_any_true", "wasm_v128_any_true", "v128_t a", "a"),
        ("wasm_i32x4_any_true", "wasm_v128_any_true", "v128_t a", "a"),
    ]
    for e in ("i8", "u8", "i16", "u16"):
        for op in ("add", "sub"):
            old.append(
                (f"{vname(e)}_{op}_saturate", f"{vname(e)}_{op}_sat", "v128_t a, v128_t b", "a, b")
            )
    for e in ("i16", "i32"):
        for src in (narrow(e), "u" + narrow(e)[1:]):
            new = e if src[0] == "i" else ut(e)
            for half in ("low", "high"):
                old.append(
                    (
                        f"{vname(e)}_widen_{half}_{src}x{lanes(src)}",
                        f"{vname(new)}_extend_{half}_{src}x{lanes(src)}",
                        "v128_t a",
                        "a",
                    )
                )
    for e in ("i32", "u32"):
        old.append(
            (f"{vname(e)}_trunc_saturate_f32x4", f"{vname(e)}_trunc_sat_f32x4", "v128_t a", "a")
        )
    for nm, new, params, args in old:
        ret = "bool" if new == "wasm_v128_any_true" else "v128_t"
        fn(
            ret,
            nm,
            params,
            f"return {new}({args});",
            f'__attribute__((__deprecated__("use {new} instead")))',
        )
    for e in SIGNED:
        w(f"#define wasm_v{e[1:]}x{lanes(e)}_shuffle {vname(e)}_shuffle")

    w("")
    w("#endif")
    return NAMES[helpers:]


names = emit()
text = "\n".join(out) + "\n"
dest = sys.argv[1] if len(sys.argv) > 1 else "crates/rucc-session/runtime/include/wasm_simd128.h"
open(dest, "w").write(text)
