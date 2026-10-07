#!/usr/bin/env python3
"""Writes a C program that calls every function and macro of the wasm_simd128.h of rucc, over a set
of vectors with the edge values of each lane type, and prints the name of each one with a hash of
all that it gave back. The program compiles against the header of clang and against the header of
rucc, and the two must print the same lines. A name that the header of clang does not have stops
the build against it.

usage: gen.py crates/rucc-session/runtime/include/wasm_simd128.h > simd.c

The names come from the header, so a function added to it is called here with no change."""

import re
import struct
import sys

text = open(sys.argv[1]).read()

# The vectors every function is called over, as 16 bytes each. Each has the edge values of one
# lane type, so a function of that type meets them and a function of another type meets bits.
POOL = []


def pack(fmt, *values):
    POOL.append(struct.pack("<" + fmt, *values))


def bits32(*values):
    pack("4I", *values)


def bits64(*values):
    pack("2Q", *values)


nan, inf = float("nan"), float("inf")
POOL.append(bytes(16))
POOL.append(bytes([0xFF] * 16))
POOL.append(bytes(range(16)))
POOL.append(bytes([0x80, 0x7F, 0x00, 0xFF, 0x01, 0xFE, 0x81, 0x7E] * 2))
pack("8h", -32768, 32767, -1, 0, 1, 0x4000, -0x4000, 12345)
pack("8h", 255, 256, -129, 128, -32767, 100, -100, 0x7F00)
pack("4i", -(2**31), 2**31 - 1, -1, 7)
pack("4i", 65535, 65536, -65536, 0x40000000)
pack("2q", -(2**63), 2**63 - 1)
pack("2q", -1, 2**32)
pack("4f", 0.0, -0.0, 1.5, -2.5)
pack("4f", 0.5, -0.5, 2.5, 3.5)
pack("4f", nan, -nan, inf, -inf)
pack("4f", 1e20, -1e20, 8388609.0, -8388607.5)
pack("4f", 2147483648.0, -2147483904.0, 4294967296.0, 0.9999)
bits32(0x7F800001, 0x7FC00001, 0x00000001, 0x80800000)
pack("4f", -1.5, 1e-3, 65504.0, -0.4)
pack("2d", 0.0, -0.0)
pack("2d", 2.5, -3.5)
pack("2d", nan, inf)
pack("2d", -inf, 4503599627370497.0)
pack("2d", 1e300, -1e-310)
pack("2d", 2147483647.5, -2147483648.9)
pack("2d", 4294967295.5, -0.5)
bits64(0x7FF0000000000001, 0xFFF8000000000001)
seed = 0x2545F4914F6CDD1D
for _ in range(4):
    out = []
    for _ in range(16):
        seed = (seed * 6364136223846793005 + 1442695040888963407) % 2**64
        out.append(seed >> 56)
    POOL.append(bytes(out))

# The relaxed functions have more than one answer for some inputs, and an engine gives the one of
# its machine, so they are called only over inputs that have one answer. The multiply adds take
# products that are exact, so fused and not fused are the same.
RELAXED = {
    "f32_madd": [(1, -2, 3.5, 100), (0.25, 4, -8, 1e3), (-0.5, 0.75, 6, -12), (3, 5, 7, -1)],
    "f64_madd": [(1, -2), (0.25, 4), (-0.5, 0.75), (3, 1e3)],
    "f32_minmax": [
        (1.5, -2, 3e10, -1e-3),
        (-1.5, 2, 1e-30, 7),
        (inf, -inf, 5, -5),
        (2, 2, -7, 0.5),
    ],
    "f64_minmax": [(1.5, -2), (-1.5, 1e-300), (inf, -inf), (2, 2)],
    "f32_trunc_s": [(0, -0.9, 1.5, 2147483520.0), (-2147483648.0, 1e9, -1e9, 3.7)],
    "f32_trunc_u": [(0, 0.9, 4294967040.0, 1e9), (1.5, 2.5, 3.5, 65536.25)],
    "f64_trunc_s": [(1.5, -2.5), (2147483647.0, -2147483648.0)],
    "f64_trunc_u": [(4294967295.0, 0.5), (1e9, 7.9)],
}


def vec(fmt, values):
    return struct.pack("<" + fmt, *values)


def init(data):
    return "{" + ", ".join(str(b) for b in data) + "}"


# The functions, with their result and parameters as the header writes them.
FUNCS = []
for m in re.finditer(
    r"static __inline__ (?:__attribute__\(\(__deprecated__\(\"[^\"]*\"\)\)\)\n)?"
    r"(\w+) (wasm_\w+)\(([^)]*)\)",
    text,
):
    ret, name, params = m.group(1), m.group(2), " ".join(m.group(3).split())
    if "f16" in name:
        continue
    params = [p.strip() for p in params.split(",")] if params else []
    FUNCS.append((name, ret, params))
MACROS = sorted(set(re.findall(r"#define (wasm_\w+_shuffle)\b", text)))
if not FUNCS or not MACROS:
    sys.exit("no functions found in " + sys.argv[1])


def lane_count(name):
    m = re.match(r"wasm_[iuf](\d+)x(\d+)_", name)
    if m:
        return int(m.group(2))
    m = re.match(r"wasm_v(\d+)x(\d+)_", name)
    if m:
        return int(m.group(2))
    m = re.match(r"wasm_v128_(?:load|store)(\d+)_lane", name)
    return 128 // int(m.group(1)) if m else 0


def float_result(name):
    """The lane type of a result whose NaN lanes an engine may give with any sign and payload,
    which the program makes one NaN before it hashes them."""
    ops = r"(sqrt|ceil|floor|trunc|nearest|add|sub|mul|div|min|max|relaxed_\w+)$"
    if re.match(r"wasm_f32x4_" + ops, name) or name == "wasm_f32x4_demote_f64x2_zero":
        return "f32"
    if re.match(r"wasm_f64x2_" + ops, name) or name == "wasm_f64x2_promote_low_f32x4":
        return "f64"
    return None


def scalar_getter(ctype):
    return {
        "int8_t": "g_i8",
        "uint8_t": "g_u8",
        "int16_t": "g_i16",
        "uint16_t": "g_u16",
        "int32_t": "g_i32",
        "uint32_t": "g_u32",
        "int64_t": "g_i64",
        "uint64_t": "g_u64",
        "float": "g_f32",
        "double": "g_f64",
    }[ctype]


FORMATS = {
    "int8_t": "b",
    "uint8_t": "B",
    "int16_t": "h",
    "uint16_t": "H",
    "int32_t": "i",
    "uint32_t": "I",
    "int64_t": "q",
    "uint64_t": "Q",
    "float": "f",
    "double": "d",
}


def literal(ctype, value):
    """A constant of the type in C, as clang asks the const forms to be given. A NaN has no
    constant, so it is None."""
    if ctype in ("float", "double"):
        if value != value:
            return None
        suffix = "f" if ctype == "float" else ""
        if value in (inf, -inf):
            return ("-" if value < 0 else "") + f"__builtin_inf{suffix}()"
        return f"{value.hex()}{suffix}"
    if ctype == "int64_t" and value == -(2**63):
        return "(-9223372036854775807ll - 1)"
    if ctype == "int32_t" and value == -(2**31):
        return "(-2147483647 - 1)"
    suffix = {"int64_t": "ll", "uint64_t": "ull", "uint32_t": "u"}.get(ctype, "")
    return f"{value}{suffix}"


def lanes_of(ctype, data):
    fmt = FORMATS[ctype]
    count = 16 // struct.calcsize(fmt)
    return [literal(ctype, v) for v in struct.unpack(f"<{count}{fmt}", data)]


w = print

w("/* Written by tests/wasm-simd/gen.py from a wasm_simd128.h. Do not edit. */")
w("#include <stdint.h>")
w("#include <stdio.h>")
w("#include <string.h>")
w("#include <wasm_simd128.h>")
w("")
w(f"#define N {len(POOL)}")
w("static const unsigned char pool[N + 1][16] __attribute__((aligned(16))) = {")
for data in POOL + [bytes(16)]:
    w(f"  {init(data)},")
w("};")
for key, rows in RELAXED.items():
    fmt = "4f" if key.startswith("f32") else "2d"
    w(f"static const unsigned char r_{key}[{len(rows)}][16] = {{")
    for row in rows:
        w(f"  {init(vec(fmt, row))},")
    w("};")
w(
    """
static uint64_t hash;

static void eat(const void *p, size_t n) {
  const unsigned char *b = p;
  for (size_t i = 0; i < n; i++) {
    hash ^= b[i];
    hash *= 0x100000001b3ull;
  }
}

static void start(void) { hash = 0xcbf29ce484222325ull; }
static void report(const char *name) { printf("%s %016llx\\n", name, (unsigned long long)hash); }

static v128_t at(const unsigned char (*set)[16], int i) {
  v128_t v;
  memcpy(&v, set[i], 16);
  return v;
}
static v128_t P(int i) { return at(pool, i); }

/* A result made the same whatever NaN an engine gives in a lane. */
static void eat_f32(v128_t v) {
  uint32_t l[4];
  memcpy(l, &v, 16);
  for (int i = 0; i < 4; i++)
    if ((l[i] & 0x7fffffff) > 0x7f800000) l[i] = 0x7fc00000;
  eat(l, 16);
}
static void eat_f64(v128_t v) {
  uint64_t l[2];
  memcpy(l, &v, 16);
  for (int i = 0; i < 2; i++)
    if ((l[i] & 0x7fffffffffffffffull) > 0x7ff0000000000000ull) l[i] = 0x7ff8000000000000ull;
  eat(l, 16);
}
static void eat_v(v128_t v) { eat(&v, 16); }

/* A lane of a vector of the pool, as a scalar of the type a function takes. */
#define GET(name, type) \\
  static type name(int i, int k) { type t; memcpy(&t, pool[i] + k * sizeof t, sizeof t); return t; }
GET(g_i8, int8_t)
GET(g_u8, uint8_t)
GET(g_i16, int16_t)
GET(g_u16, uint16_t)
GET(g_i32, int32_t)
GET(g_u32, uint32_t)
GET(g_i64, int64_t)
GET(g_u64, uint64_t)
GET(g_f32, float)
GET(g_f64, double)

/* The inputs with one answer for the relaxed functions: the swizzle indices below 16 or with the
 * top bit set, the lane masks all ones or all zeros, no -32768 in a Q15 multiply, and a second
 * operand of a dot product below 128. */
static v128_t swizzle_index(v128_t v) { return v & (v128_t)wasm_i8x16_splat((int8_t)0x8f); }
static v128_t lane_mask(v128_t v, int bits) {
  unsigned char b[16];
  memcpy(b, &v, 16);
  for (int i = 0; i < 16; i += bits / 8) {
    unsigned char top = b[i + bits / 8 - 1] & 0x80 ? 0xff : 0;
    memset(b + i, top, bits / 8);
  }
  memcpy(&v, b, 16);
  return v;
}
static v128_t no_q15_min(v128_t v) {
  int16_t l[8];
  memcpy(l, &v, 16);
  for (int i = 0; i < 8; i++)
    if (l[i] == -32768) l[i] = -32767;
  memcpy(&v, l, 16);
  return v;
}
static v128_t seven_bits(v128_t v) { return v & (v128_t)wasm_i8x16_splat(0x7f); }

static const unsigned char *const bytes = &pool[0][0];
static const uint32_t shifts[] = {0, 1, 7, 8, 15, 16, 31, 33, 63, 64, 100, 0xffffffff};
"""
)


def eat(name, ret, expr):
    if ret == "v128_t":
        kind = float_result(name)
        return f"eat_{kind or 'v'}({expr});"
    return f"{{ {ret} t = {expr}; eat(&t, sizeof t); }}"


calls = []


def body(name, lines):
    calls.append(name)
    w(f"static void t_{name}(void) {{")
    w("  start();")
    for line in lines:
        w("  " + line)
    w(f'  report("{name}");')
    w("}")


for name, ret, params in FUNCS:
    types = [re.sub(r"\s*\*?\s*__\w+$", "", p) + ("*" if "*" in p else "") for p in params]
    sig = ", ".join(types)
    n = lane_count(name)
    if "relaxed" in name:
        if name.endswith("_relaxed_swizzle"):
            lines = [
                "for (int i = 0; i < N; i++) for (int j = 0; j < N; j++)",
                "  " + eat(name, ret, f"{name}(P(i), swizzle_index(P(j)))"),
            ]
        elif name.endswith("_relaxed_laneselect"):
            bits = 128 // n
            lines = [
                "for (int i = 0; i < N; i++) for (int j = 0; j < N; j++)",
                "  " + eat(name, ret, f"{name}(P(i), P(j), lane_mask(P((i + j + 1) % N), {bits}))"),
            ]
        elif name.endswith("_relaxed_q15mulr"):
            lines = [
                "for (int i = 0; i < N; i++) for (int j = 0; j < N; j++)",
                "  " + eat(name, ret, f"{name}(no_q15_min(P(i)), no_q15_min(P(j)))"),
            ]
        elif name.endswith("_relaxed_dot_i8x16_i7x16"):
            lines = [
                "for (int i = 0; i < N; i++) for (int j = 0; j < N; j++)",
                "  " + eat(name, ret, f"{name}(P(i), seven_bits(P(j)))"),
            ]
        elif name.endswith("_relaxed_dot_i8x16_i7x16_add"):
            lines = [
                "for (int i = 0; i < N; i++) for (int j = 0; j < N; j++)",
                "  " + eat(name, ret, f"{name}(P(i), seven_bits(P(j)), P((i + j + 1) % N))"),
            ]
        elif re.search(r"_relaxed_n?madd$", name):
            key = name[5:8] + "_madd"
            k = len(RELAXED[key])
            lines = [
                f"for (int i = 0; i < {k}; i++) for (int j = 0; j < {k}; j++) "
                f"for (int l = 0; l < {k}; l++)",
                "  " + eat(name, ret, f"{name}(at(r_{key}, i), at(r_{key}, j), at(r_{key}, l))"),
            ]
        elif re.search(r"_relaxed_(min|max)$", name):
            key = name[5:8] + "_minmax"
            k = len(RELAXED[key])
            lines = [
                f"for (int i = 0; i < {k}; i++) for (int j = 0; j < {k}; j++)",
                "  " + eat(name, ret, f"{name}(at(r_{key}, i), at(r_{key}, j))"),
            ]
        elif "_relaxed_trunc_" in name:
            src = "f32" if name.endswith("f32x4") else "f64"
            key = f"{src}_trunc_{'u' if name.startswith('wasm_u') else 's'}"
            k = len(RELAXED[key])
            lines = [f"for (int i = 0; i < {k}; i++) " + eat(name, ret, f"{name}(at(r_{key}, i))")]
        else:
            sys.exit(f"no inputs for {name}")
    elif sig == "const void*":
        lines = ["for (int i = 0; i < N; i++) " + eat(name, ret, f"{name}(bytes + 16 * i + 3)")]
    elif sig == "const void*, v128_t, int":
        lines = ["for (int i = 0; i < N; i++) {"]
        for k in range(n):
            lines.append("  " + eat(name, ret, f"{name}(bytes + 16 * i + 5, P((i + 1) % N), {k})"))
        lines.append("}")
    elif sig == "void*, v128_t":
        lines = [
            "for (int i = 0; i < N; i++) {",
            "  unsigned char b[32] = {0};",
            f"  {name}(b + 3, P(i));",
            "  eat(b, 32);",
            "}",
        ]
    elif sig == "void*, v128_t, int":
        lines = ["for (int i = 0; i < N; i++) {", "  unsigned char b[16] = {0};"]
        for k in range(n):
            lines.append(f"  {name}(b + 3, P(i), {k});")
            lines.append("  eat(b, 16);")
        lines.append("}")
    elif sig == "v128_t":
        lines = ["for (int i = 0; i < N; i++) " + eat(name, ret, f"{name}(P(i))")]
    elif sig == "v128_t, v128_t":
        lines = [
            "for (int i = 0; i < N; i++) for (int j = 0; j < N; j++)",
            "  " + eat(name, ret, f"{name}(P(i), P(j))"),
        ]
    elif sig == "v128_t, v128_t, v128_t":
        lines = [
            "for (int i = 0; i < N; i++) for (int j = 0; j < N; j++)",
            "  " + eat(name, ret, f"{name}(P(i), P(j), P((i + j + 1) % N))"),
        ]
    elif sig == "v128_t, uint32_t":
        lines = [
            "for (int i = 0; i < N; i++) for (int j = 0; j < 12; j++)",
            "  " + eat(name, ret, f"{name}(P(i), shifts[j])"),
        ]
    elif sig == "v128_t, int":
        lines = ["for (int i = 0; i < N; i++) {"]
        for k in range(n):
            lines.append("  " + eat(name, ret, f"{name}(P(i), {k})"))
        lines.append("}")
    elif re.fullmatch(r"v128_t, int, \w+", sig):
        get = scalar_getter(types[2])
        lines = ["for (int i = 0; i < N; i++) {"]
        for k in range(n):
            lines.append(
                "  "
                + eat(
                    name,
                    ret,
                    f"{name}(P(i), {k}, {get}((i + 1) % N, {k % (16 // (128 // n // 8))}))",
                )
            )
        lines.append("}")
    elif "_const" in name:
        lines = []
        for data in POOL:
            values = lanes_of(types[0], data)
            if None in values:
                continue
            if len(types) == 1:
                lines += [eat(name, ret, f"{name}({v})") for v in values[:2]]
            else:
                lines.append(eat(name, ret, f"{name}({', '.join(values)})"))
    elif len(types) == n and len(set(types)) == 1:
        get = scalar_getter(types[0])
        args = ", ".join(f"{get}(i, {k})" for k in range(n))
        lines = ["for (int i = 0; i < N; i++) " + eat(name, ret, f"{name}({args})")]
    elif len(types) == 1:
        get = scalar_getter(types[0])
        lines = [
            "for (int i = 0; i < N; i++) for (int k = 0; k < 2; k++)",
            "  " + eat(name, ret, f"{name}({get}(i, k))"),
        ]
    else:
        sys.exit(f"no way to call {name}({sig})")
    body(name, lines)

# The shuffles take literal lane indices, as clang requires, over both operands.
for name in MACROS:
    n = lane_count(name)
    patterns = [
        list(range(n)),
        list(range(n, 2 * n)),
        [2 * n - 1 - k for k in range(n)],
        [(k * 5 + 3) % (2 * n) for k in range(n)],
    ]
    lines = []
    for p in patterns:
        idx = ", ".join(map(str, p))
        lines.append(
            "for (int i = 0; i < N; i++) for (int j = 0; j < N; j++) "
            f"eat_v({name}(P(i), P(j), {idx}));"
        )
    body(name, lines)

w("")
w("int main(void) {")
for name in calls:
    w(f"  t_{name}();")
w("  return 0;")
w("}")
