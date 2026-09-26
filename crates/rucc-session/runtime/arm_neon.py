#!/usr/bin/env python3
"""Writes crates/rucc-session/runtime/include/arm_neon.h. Run from the repository root."""

import re
import sys

# name, C lane type, bits, kind (s signed, u unsigned, f float, p polynomial)
ELEMS = [
    ("s8", "int8_t", 8, "s"),
    ("s16", "int16_t", 16, "s"),
    ("s32", "int32_t", 32, "s"),
    ("s64", "int64_t", 64, "s"),
    ("u8", "uint8_t", 8, "u"),
    ("u16", "uint16_t", 16, "u"),
    ("u32", "uint32_t", 32, "u"),
    ("u64", "uint64_t", 64, "u"),
    ("f32", "float32_t", 32, "f"),
    ("f64", "float64_t", 64, "f"),
    ("p8", "poly8_t", 8, "p"),
    ("p16", "poly16_t", 16, "p"),
    ("p64", "poly64_t", 64, "p"),
]
BY = {e[0]: e for e in ELEMS}
PREFIX = {"s": "int", "u": "uint", "f": "float", "p": "poly"}
BUILTIN = {"s": "Int", "u": "Uint", "f": "Float", "p": "Poly"}

out = []
w = out.append


def lanes(e, q):
    return (128 if q else 64) // BY[e][2]


def vt(e, q):
    _, _, bits, kind = BY[e]
    return f"{PREFIX[kind]}{bits}x{lanes(e, q)}_t"


def ct(e):
    return BY[e][1]


def ut(e):
    """The unsigned lane of the same width, which is what a comparison answers in."""
    return "u" + e[1:]


def wide(e):
    return e[0] + str(BY[e][2] * 2)


def narrow(e):
    return e[0] + str(BY[e][2] // 2)


def name(op, e, q, suffix=""):
    return f"{op}{'q' if q else ''}{suffix}_{e}"


# The loops above make every operation at every lane type, and the ACLE leaves some of those out
# because there is no instruction for them. A program cannot name one of those, so they are not
# written.
NOT_ACLE = re.compile(
    r"^(vmulq?|vmlaq?|vmlsq?|vmaxq?|vminq?|vabdq?|vmvnq?|vmulq?_n|vmlaq?_n)_[su]64$"
    r"|^(vandq?|vorrq?|veorq?|vbicq?|vornq?)_p(8|16|64)$|^vmvnq?_p(16|64)$"
    r"|^(vmulq?_n|vmlaq?_n|vmlsq?_n)_[su]8$|^vmlaq?_n_f64$|^(vmull_n|vmlal_n|vmlsl_n)_[su]8$"
    r"|^(vaddv|vmaxv|vminv)_(f64|[su]64)$|^(vmaxvq|vminvq)_[su]64$"
    r"|^vpadd_(f64|[su]64)$|^(vzip|vuzp|vtrn)[12]_[supf]64$|^(vzip|vuzp|vtrn)q_[supf]64$"
)


# The names a function uses for its parameters and locals. They are written short above and made
# reserved here, so that a program with a macro called `a` or `lane` can still include the header.
LOCAL = re.compile(r"\b(a|b|c|x|y|p|v|lane|la|lb|k|n|m|r|t|s|lo|hi|i|j)\b")


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


def fn(ret, nm, params, body):
    if NOT_ACLE.match(nm):
        return
    head = f"static __inline__ {ret} {nm}({reserve(params)}) {{"
    if len(head) > 100:
        cut = head.rfind(", ", 0, 100)
        head = head[: cut + 1] + "\n    " + head[cut + 2 :]
    w(head)
    for stmt in body if isinstance(body, list) else statements(body):
        for line in wrap(reserve(stmt), "  "):
            w(line)
    w("}")


def lanewise(ret_e, ret_q, nm, params, expr, n):
    rt = vt(ret_e, ret_q)
    fn(
        rt,
        nm,
        params,
        [f"{rt} __r;", f"for (int i = 0; i < {n}; i++) __r[i] = {expr};", "return __r;"],
    )


INTS = [e for e in BY if BY[e][3] in "su"]
NOT64 = [e for e in INTS if BY[e][2] < 64]
SIGNED = [e for e in BY if BY[e][3] == "s"]
FLOATS = ["f32", "f64"]
NUMERIC = INTS + FLOATS
ALL = list(BY)
QS = (False, True)


INTRO = """
/* arm_neon.h, the AArch64 Advanced SIMD intrinsics.
 *
 * Written by `crates/rucc-session/runtime/arm_neon.py`, which is the file to change. Run it from
 * the root of the repository and it writes this one.
 *
 * rucc defines `__ARM_NEON` on AArch64 the way gcc does, and a program that sees it includes this
 * header and expects the intrinsics. xxhash is the one that found it missing. Every intrinsic here
 * is C over the lanes of a GNU vector, the same way `<emmintrin.h>` is written, so what a program
 * computes is what the instruction computes and what it compiles to is one lane at a time until
 * `tamnd/rucc#200` keeps vectors in registers. The types are the builtin types gcc registers on
 * AArch64, so a vector made here passes to and from code gcc compiled.
 *
 * What is here is the loads and stores including the interleaving ones, building and taking apart
 * vectors, reinterpretation, the arithmetic, logic, shifts and comparisons at every lane type the
 * ACLE has them for, the saturating add and subtract, the widening, narrowing and pairwise forms,
 * the conversions, the permutes, the table lookups over one register and the reductions across a
 * vector. Each one gives the same bytes as gcc's own header for the same inputs, which is checked
 * by calling every one of them. What is not here yet is the saturating and rounding shifts and
 * multiplies, the square roots and estimates, fused multiply add, the lane forms of multiply, the
 * half precision and bfloat types, and the crypto and dot product extensions. */
"""


def section(title):
    w("")
    w(f"/* {title} */")


def emit():
    for line in INTRO.strip().splitlines():
        w(line)
    w("")
    w("#ifndef __RUCC_ARM_NEON_H")
    w("#define __RUCC_ARM_NEON_H")
    w("")
    w("#if !defined(__aarch64__)")
    w('#error "arm_neon.h is for AArch64"')
    w("#endif")
    w("")
    w("#include <stdint.h>")
    w("")
    w("typedef float float32_t;")
    w("typedef double float64_t;")
    w("typedef uint8_t poly8_t;")
    w("typedef uint16_t poly16_t;")
    w("typedef uint64_t poly64_t;")
    w("")
    for q in QS:
        for e in ALL:
            _, _, bits, kind = BY[e]
            w(f"typedef __{BUILTIN[kind]}{bits}x{lanes(e, q)}_t {vt(e, q)};")
    w("")
    for q in QS:
        for e in ALL:
            v = vt(e, q)
            for n in (2, 3, 4):
                s = v[:-2] + f"x{n}_t"
                w(f"typedef struct {s} {{ {v} val[{n}]; }} {s};")

    section("Loads and stores. Through memcpy, since neither has to be aligned.")
    for q in QS:
        for e in ALL:
            v, c, n = vt(e, q), ct(e), lanes(e, q)
            fn(
                v,
                name("vld1", e, q),
                f"const {c} *p",
                f"{v} r; __builtin_memcpy(&r, p, sizeof r); return r;",
            )
            fn("void", name("vst1", e, q), f"{c} *p, {v} v", "__builtin_memcpy(p, &v, sizeof v);")
            lanewise(e, q, name("vld1", e, q, "_dup"), f"const {c} *p", "*p", n)
            fn(
                v,
                name("vld1", e, q, "_lane"),
                f"const {c} *p, {v} v, const int lane",
                "v[lane] = *p; return v;",
            )
            fn(
                "void",
                name("vst1", e, q, "_lane"),
                f"{c} *p, {v} v, const int lane",
                "*p = v[lane];",
            )
            for k in (2, 3, 4):
                s = v[:-2] + f"x{k}_t"
                fn(
                    s,
                    name("vld1", e, q) + f"_x{k}",
                    f"const {c} *p",
                    f"{s} r; __builtin_memcpy(&r, p, sizeof r); return r;",
                )
                fn(
                    "void",
                    name("vst1", e, q) + f"_x{k}",
                    f"{c} *p, {s} v",
                    "__builtin_memcpy(p, &v, sizeof v);",
                )
                # The interleaving forms: lane i of register j is element i * k + j.
                fn(
                    s,
                    name(f"vld{k}", e, q),
                    f"const {c} *p",
                    f"{s} r; for (int i = 0; i < {n}; i++) for (int j = 0; j < {k}; j++) "
                    f"r.val[j][i] = p[i * {k} + j]; return r;",
                )
                fn(
                    "void",
                    name(f"vst{k}", e, q),
                    f"{c} *p, {s} v",
                    f"for (int i = 0; i < {n}; i++) for (int j = 0; j < {k}; j++) "
                    f"p[i * {k} + j] = v.val[j][i];",
                )

    section("Building a vector and taking one apart.")
    for q in QS:
        for e in ALL:
            v, c, n = vt(e, q), ct(e), lanes(e, q)
            lanewise(e, q, name("vdup", e, q, "_n"), f"{c} x", "x", n)
            lanewise(e, q, name("vmov", e, q, "_n"), f"{c} x", "x", n)
            fn(c, name("vget", e, q, "_lane"), f"{v} v, const int lane", "return v[lane];")
            fn(
                v,
                name("vset", e, q, "_lane"),
                f"{c} x, {v} v, const int lane",
                "v[lane] = x; return v;",
            )
            for fromq in QS:
                sfx = "_laneq" if fromq else "_lane"
                lanewise(
                    e, q, name("vdup", e, q, sfx), f"{vt(e, fromq)} v, const int lane", "v[lane]", n
                )
            fn(
                v,
                name("vcopy", e, q, "_lane"),
                f"{v} a, const int la, {vt(e, False)} b, const int lb",
                "a[la] = b[lb]; return a;",
            )
            fn(
                v,
                name("vcopy", e, q, "_laneq"),
                f"{v} a, const int la, {vt(e, True)} b, const int lb",
                "a[la] = b[lb]; return a;",
            )
    for e in ALL:
        d, qv, n = vt(e, False), vt(e, True), lanes(e, False)
        lanewise(e, False, f"vget_low_{e}", f"{qv} v", "v[i]", n)
        lanewise(e, False, f"vget_high_{e}", f"{qv} v", f"v[i + {n}]", n)
        lanewise(
            e, True, f"vcombine_{e}", f"{d} lo, {d} hi", f"i < {n} ? lo[i] : hi[i - {n}]", 2 * n
        )
        fn(d, f"vcreate_{e}", "uint64_t x", f"{d} r; __builtin_memcpy(&r, &x, sizeof r); return r;")

    section("Reinterpretation, which keeps the bytes and changes what the lanes are.")
    for q in QS:
        for a in ALL:
            for b in ALL:
                if a == b:
                    continue
                va, vb = vt(a, q), vt(b, q)
                fn(
                    va,
                    f"vreinterpret{'q' if q else ''}_{a}_{b}",
                    f"{vb} v",
                    f"{va} r; __builtin_memcpy(&r, &v, sizeof r); return r;",
                )

    section("Arithmetic and logic, a lane at a time.")
    for q in QS:
        for e in NUMERIC:
            v, c, n = vt(e, q), ct(e), lanes(e, q)
            for op, sym in (("vadd", "+"), ("vsub", "-"), ("vmul", "*")):
                lanewise(e, q, name(op, e, q), f"{v} a, {v} b", f"({c})(a[i] {sym} b[i])", n)
            lanewise(e, q, name("vmul", e, q, "_n"), f"{v} a, {c} b", f"({c})(a[i] * b)", n)
            lanewise(
                e, q, name("vmla", e, q), f"{v} a, {v} b, {v} c", f"({c})(a[i] + b[i] * c[i])", n
            )
            lanewise(
                e, q, name("vmls", e, q), f"{v} a, {v} b, {v} c", f"({c})(a[i] - b[i] * c[i])", n
            )
            lanewise(
                e, q, name("vmla", e, q, "_n"), f"{v} a, {v} b, {c} c", f"({c})(a[i] + b[i] * c)", n
            )
            lanewise(e, q, name("vmax", e, q), f"{v} a, {v} b", "a[i] > b[i] ? a[i] : b[i]", n)
            lanewise(e, q, name("vmin", e, q), f"{v} a, {v} b", "a[i] < b[i] ? a[i] : b[i]", n)
            lanewise(
                e,
                q,
                name("vabd", e, q),
                f"{v} a, {v} b",
                f"({c})(a[i] > b[i] ? a[i] - b[i] : b[i] - a[i])",
                n,
            )
            ops = (("vceq", "=="), ("vcge", ">="), ("vcgt", ">"), ("vcle", "<="), ("vclt", "<"))
            u = ut(e)
            uc = ct(u)
            for op, sym in ops:
                lanewise(
                    u, q, name(op, e, q), f"{v} a, {v} b", f"a[i] {sym} b[i] ? ({uc})-1 : 0", n
                )
                if op == "vceq" or BY[e][3] != "u":
                    lanewise(
                        u, q, name(op + "z", e, q), f"{v} a", f"a[i] {sym} 0 ? ({uc})-1 : 0", n
                    )
            fn(
                c,
                name("vaddv", e, q),
                f"{v} a",
                f"{c} r = 0; for (int i = 0; i < {n}; i++) r += a[i]; return r;",
            )
            fn(
                c,
                name("vmaxv", e, q),
                f"{v} a",
                f"{c} r = a[0]; for (int i = 1; i < {n}; i++) if (a[i] > r) r = a[i]; return r;",
            )
            fn(
                c,
                name("vminv", e, q),
                f"{v} a",
                f"{c} r = a[0]; for (int i = 1; i < {n}; i++) if (a[i] < r) r = a[i]; return r;",
            )
            lanewise(
                e,
                q,
                name("vpadd", e, q),
                f"{v} a, {v} b",
                f"i < {n // 2} ? ({c})(a[2 * i] + a[2 * i + 1]) : "
                f"({c})(b[2 * (i - {n // 2})] + b[2 * (i - {n // 2}) + 1])",
                n,
            )
        for e in SIGNED + FLOATS:
            v, c, n = vt(e, q), ct(e), lanes(e, q)
            lanewise(e, q, name("vneg", e, q), f"{v} a", f"({c})-a[i]", n)
            lanewise(e, q, name("vabs", e, q), f"{v} a", f"a[i] < 0 ? ({c})-a[i] : a[i]", n)
        for e in FLOATS:
            v, c, n = vt(e, q), ct(e), lanes(e, q)
            lanewise(e, q, name("vdiv", e, q), f"{v} a, {v} b", "a[i] / b[i]", n)
        for e in INTS + ["p8", "p16", "p64"]:
            v, c, n = vt(e, q), ct(e), lanes(e, q)
            for op, expr in (
                ("vand", "a[i] & b[i]"),
                ("vorr", "a[i] | b[i]"),
                ("veor", "a[i] ^ b[i]"),
                ("vbic", "a[i] & ~b[i]"),
                ("vorn", "a[i] | ~b[i]"),
            ):
                lanewise(e, q, name(op, e, q), f"{v} a, {v} b", f"({c})({expr})", n)
            lanewise(e, q, name("vmvn", e, q), f"{v} a", f"({c})~a[i]", n)
            u, uc = ut(e), ct(ut(e))
            lanewise(u, q, name("vtst", e, q), f"{v} a, {v} b", f"(a[i] & b[i]) ? ({uc})-1 : 0", n)
        for e in ALL:
            v, uv = vt(e, q), vt(ut(e), q)
            fn(
                v,
                name("vbsl", e, q),
                f"{uv} m, {v} a, {v} b",
                f"{uv} x, y; __builtin_memcpy(&x, &a, sizeof x); "
                f"__builtin_memcpy(&y, &b, sizeof y); x = (x & m) | (y & ~m); {v} r; "
                "__builtin_memcpy(&r, &x, sizeof r); return r;",
            )
        for e in INTS:
            v, c, n = vt(e, q), ct(e), lanes(e, q)
            bits = BY[e][2]
            if bits < 64:
                big = "int64_t"
                mx = f"(({big})1 << {bits - (1 if BY[e][3] == 's' else 0)}) - 1"
                mn = f"-(({big})1 << {bits - 1})" if BY[e][3] == "s" else "0"
                for op, sym in (("vqadd", "+"), ("vqsub", "-")):
                    expr = (
                        f"({c})(({big})a[i] {sym} ({big})b[i] > {mx} ? {mx} : "
                        f"({big})a[i] {sym} ({big})b[i] < {mn} ? {mn} : "
                        f"({big})a[i] {sym} ({big})b[i])"
                    )
                    lanewise(e, q, name(op, e, q), f"{v} a, {v} b", expr, n)
                lanewise(
                    e,
                    q,
                    name("vhadd", e, q),
                    f"{v} a, {v} b",
                    f"({c})((({big})a[i] + b[i]) >> 1)",
                    n,
                )
                lanewise(
                    e,
                    q,
                    name("vrhadd", e, q),
                    f"{v} a, {v} b",
                    f"({c})((({big})a[i] + b[i] + 1) >> 1)",
                    n,
                )
            elif BY[e][3] == "s":
                # Nothing is wider than 64 bits to work it out in, so the test is on the operands.
                add = (
                    "b[i] > 0 && a[i] > INT64_MAX - b[i] ? INT64_MAX : "
                    "b[i] < 0 && a[i] < INT64_MIN - b[i] ? INT64_MIN : a[i] + b[i]"
                )
                sub = (
                    "b[i] < 0 && a[i] > INT64_MAX + b[i] ? INT64_MAX : "
                    "b[i] > 0 && a[i] < INT64_MIN + b[i] ? INT64_MIN : a[i] - b[i]"
                )
                lanewise(e, q, name("vqadd", e, q), f"{v} a, {v} b", add, n)
                lanewise(e, q, name("vqsub", e, q), f"{v} a, {v} b", sub, n)
            else:
                lanewise(
                    e,
                    q,
                    name("vqadd", e, q),
                    f"{v} a, {v} b",
                    "a[i] + b[i] < a[i] ? UINT64_MAX : a[i] + b[i]",
                    n,
                )
                lanewise(
                    e, q, name("vqsub", e, q), f"{v} a, {v} b", "a[i] < b[i] ? 0 : a[i] - b[i]", n
                )
            # Shifts by a constant, and by a vector whose lanes are signed and shift right when
            # negative.
            lanewise(
                e,
                q,
                name("vshl", e, q, "_n"),
                f"{v} a, const int n",
                f"({c})((uint64_t)a[i] << n)",
                n,
            )
            # A shift right by the whole width is allowed, and C does not define it, so it is the
            # sign or zero written out.
            fill = "a[i] < 0 ? -1 : 0" if BY[e][3] == "s" else "0"
            lanewise(
                e,
                q,
                name("vshr", e, q, "_n"),
                f"{v} a, const int n",
                f"n == {bits} ? ({c})({fill}) : ({c})(a[i] >> n)",
                n,
            )
            lanewise(
                e,
                q,
                name("vsra", e, q, "_n"),
                f"{v} a, {v} b, const int n",
                f"({c})(a[i] + (n == {bits} ? ({c})({fill.replace('a[', 'b[')}) : "
                f"({c})(b[i] >> n)))",
                n,
            )
            # By a vector, the count is the low byte of each lane taken as signed, and a negative
            # count shifts right.
            sv = vt("s" + str(bits), q)
            lanewise(
                e,
                q,
                name("vshl", e, q),
                f"{v} a, {sv} b",
                f"(int8_t)b[i] >= {bits} ? 0 : (int8_t)b[i] <= -{bits} ? ({c})({fill}) : "
                f"(int8_t)b[i] >= 0 ? ({c})((uint64_t)a[i] << (int8_t)b[i]) : "
                f"({c})(a[i] >> -(int8_t)b[i])",
                n,
            )
            ones = f"(({'uint64_t'})-1 >> {64 - bits})"
            lanewise(
                e,
                q,
                name("vsli", e, q, "_n"),
                f"{v} a, {v} b, const int n",
                f"({c})(((uint64_t)b[i] << n) | ((uint64_t)a[i] & ({ones} >> ({bits} - n))))",
                n,
            )
            lanewise(
                e,
                q,
                name("vsri", e, q, "_n"),
                f"{v} a, {v} b, const int n",
                f"({c})((((uint64_t)b[i] & {ones}) >> n) | ((uint64_t)a[i] & ~({ones} >> n)))",
                n,
            )
            if bits < 64:
                lanewise(
                    e,
                    q,
                    name("vclz", e, q),
                    f"{v} a",
                    f"({c})(a[i] == 0 ? {bits} : "
                    f"__builtin_clz((uint32_t)a[i] & {ones}) - {32 - bits})",
                    n,
                )
            if bits == 8:
                lanewise(
                    e, q, name("vcnt", e, q), f"{v} a", f"({c})__builtin_popcount((uint8_t)a[i])", n
                )
                lanewise(
                    e,
                    q,
                    name("vrbit", e, q),
                    f"{v} a",
                    # The bit reversal of a byte by multiplication, from Bit Twiddling Hacks.
                    f"({c})(((((uint8_t)a[i] * 0x0802u & 0x22110u) | "
                    f"((uint8_t)a[i] * 0x8020u & 0x88440u)) * 0x10101u >> 16) & 0xff)",
                    n,
                )

    section("Widening, narrowing and the long forms.")
    for e in INTS:
        if BY[e][2] == 64:
            continue
        W, c, wc = wide(e), ct(e), ct(wide(e))
        d, qv, wv, n = vt(e, False), vt(e, True), vt(wide(e), True), lanes(e, False)
        lanewise(W, True, f"vmovl_{e}", f"{d} a", f"({wc})a[i]", n)
        lanewise(W, True, f"vmovl_high_{e}", f"{qv} a", f"({wc})a[i + {n}]", n)
        lanewise(e, False, f"vmovn_{W}", f"{wv} a", f"({c})a[i]", n)
        lanewise(
            e,
            True,
            f"vmovn_high_{W}",
            f"{d} lo, {wv} a",
            f"i < {n} ? lo[i] : ({c})a[i - {n}]",
            2 * n,
        )
        lanewise(e, False, f"vshrn_n_{W}", f"{wv} a, const int k", f"({c})(a[i] >> k)", n)
        lanewise(
            e,
            False,
            f"vrshrn_n_{W}",
            f"{wv} a, const int k",
            f"({c})((a[i] + (({wc})1 << (k - 1))) >> k)",
            n,
        )
        for op, sym in (("vaddl", "+"), ("vsubl", "-"), ("vmull", "*")):
            lanewise(
                W, True, f"{op}_{e}", f"{d} a, {d} b", f"({wc})(({wc})a[i] {sym} ({wc})b[i])", n
            )
            lanewise(
                W,
                True,
                f"{op}_high_{e}",
                f"{qv} a, {qv} b",
                f"({wc})(({wc})a[i + {n}] {sym} ({wc})b[i + {n}])",
                n,
            )
        lanewise(W, True, f"vmull_n_{e}", f"{d} a, {c} b", f"({wc})(({wc})a[i] * ({wc})b)", n)
        lanewise(
            W,
            True,
            f"vabdl_{e}",
            f"{d} a, {d} b",
            f"({wc})(a[i] > b[i] ? ({wc})a[i] - b[i] : ({wc})b[i] - a[i])",
            n,
        )
        for op, sym in (("vaddw", "+"), ("vsubw", "-")):
            lanewise(W, True, f"{op}_{e}", f"{wv} a, {d} b", f"({wc})(a[i] {sym} ({wc})b[i])", n)
            lanewise(
                W,
                True,
                f"{op}_high_{e}",
                f"{wv} a, {qv} b",
                f"({wc})(a[i] {sym} ({wc})b[i + {n}])",
                n,
            )
        for op, sym in (("vmlal", "+"), ("vmlsl", "-")):
            lanewise(
                W,
                True,
                f"{op}_{e}",
                f"{wv} a, {d} b, {d} c",
                f"({wc})(a[i] {sym} ({wc})b[i] * ({wc})c[i])",
                n,
            )
            lanewise(
                W,
                True,
                f"{op}_high_{e}",
                f"{wv} a, {qv} b, {qv} c",
                f"({wc})(a[i] {sym} ({wc})b[i + {n}] * ({wc})c[i + {n}])",
                n,
            )
            lanewise(
                W,
                True,
                f"{op}_n_{e}",
                f"{wv} a, {d} b, {c} c",
                f"({wc})(a[i] {sym} ({wc})b[i] * ({wc})c)",
                n,
            )
        for q in QS:
            v, m = vt(e, q), lanes(e, q)
            pv = vt(W, q)
            lanewise(
                W,
                q,
                name("vpaddl", e, q),
                f"{v} a",
                f"({wc})(({wc})a[2 * i] + a[2 * i + 1])",
                m // 2,
            )
            lanewise(
                W,
                q,
                name("vpadal", e, q),
                f"{pv} s, {v} a",
                f"({wc})(s[i] + ({wc})a[2 * i] + a[2 * i + 1])",
                m // 2,
            )
            fn(
                wc,
                name("vaddlv", e, q),
                f"{v} a",
                f"{wc} r = 0; for (int i = 0; i < {m}; i++) r += a[i]; return r;",
            )
        lanewise(W, True, f"vshll_n_{e}", f"{d} a, const int k", f"({wc})(({wc})a[i] << k)", n)
        lanewise(
            W,
            True,
            f"vshll_high_n_{e}",
            f"{qv} a, const int k",
            f"({wc})(({wc})a[i + {n}] << k)",
            n,
        )
    # A carry-less product: each set bit of one side adds the other shifted, with exclusive or.
    fn(
        "poly16x8_t",
        "vmull_p8",
        "poly8x8_t a, poly8x8_t b",
        [
            "poly16x8_t __r = {0};",
            "for (int i = 0; i < 8; i++)",
            "  for (int k = 0; k < 8; k++)",
            "    if (b[i] >> k & 1) __r[i] ^= (uint16_t)(a[i] << k);",
            "return __r;",
        ],
    )

    section("Conversions between integers and floats, where a value out of range saturates.")
    for q in QS:
        for f, s, u in (("f32", "s32", "u32"), ("f64", "s64", "u64")):
            fv, n = vt(f, q), lanes(f, q)
            fc = ct(f)
            for i in (s, u):
                ic = ct(i)
                lanewise(
                    f,
                    q,
                    name("vcvt", f, q, "").replace(f"_{f}", f"_{f}_{i}"),
                    f"{vt(i, q)} a",
                    f"({fc})a[i]",
                    n,
                )
                lo = "0" if i[0] == "u" else f"-0x1p{BY[i][2] - 1}"
                hi = f"0x1p{BY[i][2] - (0 if i[0] == 'u' else 1)}"
                mx = (
                    f"({ic})~({ic})0" if i[0] == "u" else f"({ic})((uint64_t)-1 >> {65 - BY[i][2]})"
                )
                mn = "0" if i[0] == "u" else f"({ic})(-{mx} - 1)"
                lanewise(
                    i,
                    q,
                    name("vcvt", i, q, "").replace(f"_{i}", f"_{i}_{f}"),
                    f"{fv} a",
                    f"a[i] != a[i] ? 0 : a[i] >= {hi} ? {mx} : a[i] <= {lo} ? {mn} : ({ic})a[i]",
                    n,
                )
    lanewise("f64", True, "vcvt_f64_f32", "float32x2_t a", "(float64_t)a[i]", 2)
    lanewise("f32", False, "vcvt_f32_f64", "float64x2_t a", "(float32_t)a[i]", 2)

    section("Permutes.")
    for q in QS:
        for e in ALL:
            v, n = vt(e, q), lanes(e, q)
            h = n // 2
            lanewise(
                e,
                q,
                name("vext", e, q),
                f"{v} a, {v} b, const int k",
                f"i + k < {n} ? a[i + k] : b[i + k - {n}]",
                n,
            )
            lanewise(e, q, name("vzip1", e, q), f"{v} a, {v} b", "i % 2 ? b[i / 2] : a[i / 2]", n)
            lanewise(
                e,
                q,
                name("vzip2", e, q),
                f"{v} a, {v} b",
                f"i % 2 ? b[{h} + i / 2] : a[{h} + i / 2]",
                n,
            )
            lanewise(
                e,
                q,
                name("vuzp1", e, q),
                f"{v} a, {v} b",
                f"i < {h} ? a[2 * i] : b[2 * (i - {h})]",
                n,
            )
            lanewise(
                e,
                q,
                name("vuzp2", e, q),
                f"{v} a, {v} b",
                f"i < {h} ? a[2 * i + 1] : b[2 * (i - {h}) + 1]",
                n,
            )
            lanewise(e, q, name("vtrn1", e, q), f"{v} a, {v} b", "i % 2 ? b[i - 1] : a[i]", n)
            lanewise(e, q, name("vtrn2", e, q), f"{v} a, {v} b", "i % 2 ? b[i] : a[i + 1]", n)
            if n >= 2:
                s = v[:-2] + "x2_t"
                for op in ("zip", "uzp", "trn"):
                    fn(
                        s,
                        name(f"v{op}", e, q),
                        f"{v} a, {v} b",
                        f"{s} r; r.val[0] = {name(f'v{op}1', e, q)}(a, b); "
                        f"r.val[1] = {name(f'v{op}2', e, q)}(a, b); return r;",
                    )
            bits = BY[e][2]
            for span in (16, 32, 64):
                if span > bits:
                    k = span // bits
                    lanewise(
                        e,
                        q,
                        name(f"vrev{span}", e, q),
                        f"{v} a",
                        f"a[i - i % {k} + {k} - 1 - i % {k}]",
                        n,
                    )
    for q in QS:
        for e in ("u8", "s8", "p8"):
            v, n = vt(e, q), lanes(e, q)
            t = vt(e, True)
            lanewise(
                e, q, name("vqtbl1", e, q), f"{t} t, {vt('u8', q)} x", "x[i] < 16 ? t[x[i]] : 0", n
            )
            lanewise(
                e,
                q,
                name("vqtbx1", e, q),
                f"{v} lo, {t} t, {vt('u8', q)} x",
                "x[i] < 16 ? t[x[i]] : lo[i]",
                n,
            )
    for e in ("u8", "s8", "p8"):
        x = vt("s8" if e == "s8" else "u8", False)
        lanewise(
            e,
            False,
            f"vtbl1_{e}",
            f"{vt(e, False)} t, {x} x",
            "(uint8_t)x[i] < 8 ? t[(uint8_t)x[i]] : 0",
            8,
        )

    w("")
    w("#endif")


emit()
text = "\n".join(out) + "\n"
dest = sys.argv[1] if len(sys.argv) > 1 else "crates/rucc-session/runtime/include/arm_neon.h"
open(dest, "w").write(text)
