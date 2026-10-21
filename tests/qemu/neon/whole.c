/*
 * The integer intrinsics of <arm_neon.h> and the GNU vector operators that AArch64 does on a whole
 * `v` register: the loads and stores, add, subtract, the bitwise operators, the broadcast, the
 * shifts by a constant at each end of their range, the lane compares and taking a lane out and
 * putting one in. See tamnd/rucc#3775.
 *
 * Every input is read from a volatile array, so neither compiler can work an answer out while it
 * builds, and every answer is printed as its bytes. gcc's build of this file with its own header is
 * the reference for rucc's.
 */

#include <arm_neon.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

typedef uint8_t u8v __attribute__((vector_size(16)));
typedef int8_t s8v __attribute__((vector_size(16)));
typedef uint16_t u16v __attribute__((vector_size(16)));
typedef int16_t s16v __attribute__((vector_size(16)));
typedef uint32_t u32v __attribute__((vector_size(16)));
typedef int32_t s32v __attribute__((vector_size(16)));
typedef uint64_t u64v __attribute__((vector_size(16)));
typedef int64_t s64v __attribute__((vector_size(16)));

static volatile uint8_t bytes[64] = {
	0x00, 0x01, 0x7f, 0x80, 0xff, 0xfe, 0x81, 0x55, 0xaa, 0x10, 0xef, 0x3c, 0xc3, 0x02, 0xfd, 0x40,
	0xff, 0xff, 0xff, 0x7f, 0x00, 0x00, 0x00, 0x80, 0x01, 0x00, 0x00, 0x00, 0xfe, 0xff, 0xff, 0xff,
	0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0, 0x0f, 0xed, 0xcb, 0xa9, 0x87, 0x65, 0x43, 0x21,
	0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
};

/* The sixteen bytes at `at`, read a byte at a time so the read itself is not what is tested. */
static void take(void *to, int at)
{
	uint8_t b[16];
	for (int i = 0; i < 16; i++)
		b[i] = bytes[(at + i) % 64];
	memcpy(to, b, 16);
}

static void show(const char *what, int at, const void *v)
{
	const uint8_t *b = v;
	printf("%-16s %2d ", what, at);
	for (int i = 0; i < 16; i++)
		printf("%02x", b[i]);
	printf("\n");
}

/* The intrinsics. `T` is the lane name, `V` the vector type and `B` the lane width in bits. */
#define SHIFTS(T, V, B, a, at)                                                                    \
	do {                                                                                       \
		V r;                                                                               \
		r = vshlq_n_##T(a, 0), show("vshlq_n_" #T " 0", at, &r);                           \
		r = vshlq_n_##T(a, 1), show("vshlq_n_" #T " 1", at, &r);                           \
		r = vshlq_n_##T(a, 3), show("vshlq_n_" #T " 3", at, &r);                           \
		r = vshlq_n_##T(a, B - 1), show("vshlq_n_" #T " B-1", at, &r);                     \
		r = vshrq_n_##T(a, 1), show("vshrq_n_" #T " 1", at, &r);                           \
		r = vshrq_n_##T(a, 5), show("vshrq_n_" #T " 5", at, &r);                           \
		r = vshrq_n_##T(a, B - 1), show("vshrq_n_" #T " B-1", at, &r);                     \
		r = vshrq_n_##T(a, B), show("vshrq_n_" #T " B", at, &r);                           \
	} while (0)

#define INTRINSICS(T, C, V)                                                                       \
	static void intrinsics_##T(int at)                                                         \
	{                                                                                          \
		C in[32];                                                                          \
		take(in, at);                                                                      \
		take((char *)in + 16, at + 16);                                                    \
		V a = vld1q_##T(in), b = vld1q_##T(in + 16 / sizeof(C)), r;                        \
		r = veorq_##T(a, b), show("veorq_" #T, at, &r);                                    \
		r = vandq_##T(a, b), show("vandq_" #T, at, &r);                                    \
		r = vorrq_##T(a, b), show("vorrq_" #T, at, &r);                                    \
		r = vaddq_##T(a, b), show("vaddq_" #T, at, &r);                                    \
		r = vsubq_##T(a, b), show("vsubq_" #T, at, &r);                                    \
		r = vdupq_n_##T(in[3]), show("vdupq_n_" #T, at, &r);                               \
		SHIFTS(T, V, (int)(8 * sizeof(C)), a, at);                                         \
		C out[16 / sizeof(C) + 2] = {0};                                                   \
		vst1q_##T(out + 1, veorq_##T(vaddq_##T(a, b), vshlq_n_##T(b, 1)));                 \
		show("vst1q_" #T, at, out + 1);                                                    \
	}

INTRINSICS(u8, uint8_t, uint8x16_t)
INTRINSICS(s8, int8_t, int8x16_t)
INTRINSICS(u16, uint16_t, uint16x8_t)
INTRINSICS(s16, int16_t, int16x8_t)
INTRINSICS(u32, uint32_t, uint32x4_t)
INTRINSICS(s32, int32_t, int32x4_t)
INTRINSICS(u64, uint64_t, uint64x2_t)
INTRINSICS(s64, int64_t, int64x2_t)

/* The GNU vector operators on a vector of each shape, through pointers the way the kernel holds
 * them, and as values. */
#define OPERATORS(N, V, C, B)                                                                     \
	static V N##_xor(V a, V b) { return a ^ b; }                                               \
	static void N##_into(V *p, const V *q) { *p ^= *q; }                                       \
	static V N##_loop(const V *q, int n, V a)                                                  \
	{                                                                                          \
		for (int i = 0; i < n; i++)                                                        \
			a = (a << 1) ^ q[i];                                                       \
		return a;                                                                          \
	}                                                                                          \
	static void operators_##N(int at)                                                          \
	{                                                                                          \
		V a, b, q[3], r;                                                                   \
		take(&a, at);                                                                      \
		take(&b, at + 16);                                                                 \
		for (int i = 0; i < 3; i++)                                                        \
			take(&q[i], at + 5 * i + 3);                                               \
		r = N##_xor(a, b), show(#N " ^", at, &r);                                          \
		r = a, N##_into(&r, &b), show(#N " ^=", at, &r);                                   \
		r = N##_loop(q, 3, a), show(#N " loop", at, &r);                                   \
		r = a & b, show(#N " &", at, &r);                                                  \
		r = a | b, show(#N " |", at, &r);                                                  \
		r = a + b, show(#N " +", at, &r);                                                  \
		r = a - b, show(#N " -", at, &r);                                                  \
		r = ~a, show(#N " ~", at, &r);                                                     \
		r = -a, show(#N " neg", at, &r);                                                   \
		r = a << 2, show(#N " << 2", at, &r);                                              \
		r = a >> 1, show(#N " >> 1", at, &r);                                              \
		r = a >> (B - 1), show(#N " >> B-1", at, &r);                                      \
		r = (V)(a == b), show(#N " ==", at, &r);                                           \
		r = (V)(a != b), show(#N " !=", at, &r);                                           \
		r = (V)(a < b), show(#N " <", at, &r);                                             \
		r = (V)(a <= b), show(#N " <=", at, &r);                                           \
		r = (V)(a > b), show(#N " >", at, &r);                                             \
		r = (V)(a >= b), show(#N " >=", at, &r);                                           \
		C x = a[1];                                                                        \
		r = b + x, show(#N " + lane", at, &r);                                             \
		r = a, r[0] = b[B / 8 % (16 / sizeof(C))], show(#N " insert", at, &r);             \
		r = (V){0} + (C)0x5a, show(#N " splat", at, &r);                                   \
	}

OPERATORS(u8, u8v, uint8_t, 8)
OPERATORS(s8, s8v, int8_t, 8)
OPERATORS(u16, u16v, uint16_t, 16)
OPERATORS(s16, s16v, int16_t, 16)
OPERATORS(u32, u32v, uint32_t, 32)
OPERATORS(s32, s32v, int32_t, 32)
OPERATORS(u64, u64v, uint64_t, 64)
OPERATORS(s64, s64v, int64_t, 64)

int main(void)
{
	for (int at = 0; at < 64; at += 13) {
		intrinsics_u8(at);
		intrinsics_s8(at);
		intrinsics_u16(at);
		intrinsics_s16(at);
		intrinsics_u32(at);
		intrinsics_s32(at);
		intrinsics_u64(at);
		intrinsics_s64(at);
		operators_u8(at);
		operators_s8(at);
		operators_u16(at);
		operators_s16(at);
		operators_u32(at);
		operators_s32(at);
		operators_u64(at);
		operators_s64(at);
	}
	return 0;
}
