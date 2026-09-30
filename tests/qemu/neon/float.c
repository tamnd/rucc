/*
 * The float intrinsics of <arm_neon.h> whose answer depends on the sign of a zero, on which NaN
 * comes back or on the order the lanes are added in, and the 64 bit shift and insert forms at
 * the ends of their range.
 *
 * vabs and vabd are FABS and FABD, which clear the sign bit of -0.0 and of a NaN. vmax and vmin
 * are FMAX and FMIN, which give back a NaN operand, the first signalling one quietened before the
 * first quiet one, and put -0.0 below +0.0, and the across-lanes forms take the lanes in pairs
 * with the same rule. vaddv is FADDP, which adds (a0 + a1) + (a2 + a3) with nothing to start
 * from, so a vector of -0.0 sums to -0.0. vsli_n by 0 keeps none of the destination and vsri_n by
 * 64 keeps all of it. See tamnd/rucc#2300 to #2303.
 *
 * Every value is printed as its bits and read from a volatile array, so neither compiler can
 * work an answer out while it builds, and gcc's build of this file with its own header is the
 * reference for rucc's.
 */

#include <arm_neon.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

static volatile uint32_t singles[] = {
	0x00000000, 0x80000000, 0x7fc00000, 0xffc00001, 0x7fa00002, 0xffa00003, 0x7f800000,
	0xff800000, 0x3f800000, 0xbf800000, 0x4cbebc20, 0xccbebc20, 0x00000001, 0x80000001,
};
#define SINGLES (sizeof singles / sizeof singles[0])

static volatile uint64_t doubles[] = {
	0x0000000000000000, 0x8000000000000000, 0x7ff8000000000000, 0xfff8000000000001,
	0x7ff4000000000002, 0xfff4000000000003, 0x7ff0000000000000, 0xfff0000000000000,
	0x3ff0000000000000, 0xbff0000000000000, 0x4341c37937e08000, 0xc341c37937e08000,
	0x0000000000000001, 0x8000000000000001,
};
#define DOUBLES (sizeof doubles / sizeof doubles[0])

static float32_t single(size_t i)
{
	uint32_t bits = singles[i % SINGLES];
	float32_t f;
	memcpy(&f, &bits, sizeof f);
	return f;
}

static float64_t dbl(size_t i)
{
	uint64_t bits = doubles[i % DOUBLES];
	float64_t f;
	memcpy(&f, &bits, sizeof f);
	return f;
}

static void put32(float32_t f)
{
	uint32_t bits;
	memcpy(&bits, &f, sizeof bits);
	printf(" %08x", (unsigned)bits);
}

static void put64(float64_t f)
{
	uint64_t bits;
	memcpy(&bits, &f, sizeof bits);
	printf(" %016llx", (unsigned long long)bits);
}

/* Two lanes of each pair of values, once each way round. */
#define PAIRS32(what, call)                                                                   \
	for (size_t i = 0; i < SINGLES; i++) {                                                  \
		printf("%s %zu:", what, i);                                                     \
		for (size_t j = 0; j < SINGLES; j++) {                                          \
			float32_t pa[4] = { single(i), single(j), single(j), single(i) };       \
			float32_t pb[4] = { single(j), single(i), single(i + 1), single(j + 3) }; \
			call;                                                                   \
		}                                                                               \
		printf("\n");                                                                   \
	}

#define PAIRS64(what, call)                                                                   \
	for (size_t i = 0; i < DOUBLES; i++) {                                                  \
		printf("%s %zu:", what, i);                                                     \
		for (size_t j = 0; j < DOUBLES; j++) {                                          \
			float64_t pa[2] = { dbl(i), dbl(j) };                                   \
			float64_t pb[2] = { dbl(j), dbl(i) };                                   \
			call;                                                                   \
		}                                                                               \
		printf("\n");                                                                   \
	}

#define EACH32(v, n)                                                                          \
	for (int k = 0; k < n; k++)                                                             \
		put32(v[k]);
#define EACH64(v, n)                                                                          \
	for (int k = 0; k < n; k++)                                                             \
		put64(v[k]);

static void floats(void)
{
	PAIRS32("vabs_f32", float32x2_t r = vabs_f32(vld1_f32(pa)); EACH32(r, 2))
	PAIRS32("vabsq_f32", float32x4_t r = vabsq_f32(vld1q_f32(pb)); EACH32(r, 4))
	PAIRS32("vabd_f32", float32x2_t r = vabd_f32(vld1_f32(pa), vld1_f32(pb)); EACH32(r, 2))
	PAIRS32("vabdq_f32", float32x4_t r = vabdq_f32(vld1q_f32(pa), vld1q_f32(pb)); EACH32(r, 4))
	PAIRS32("vmax_f32", float32x2_t r = vmax_f32(vld1_f32(pa), vld1_f32(pb)); EACH32(r, 2))
	PAIRS32("vmaxq_f32", float32x4_t r = vmaxq_f32(vld1q_f32(pa), vld1q_f32(pb)); EACH32(r, 4))
	PAIRS32("vmin_f32", float32x2_t r = vmin_f32(vld1_f32(pa), vld1_f32(pb)); EACH32(r, 2))
	PAIRS32("vminq_f32", float32x4_t r = vminq_f32(vld1q_f32(pa), vld1q_f32(pb)); EACH32(r, 4))
	PAIRS32("vmaxv_f32", put32(vmaxv_f32(vld1_f32(pa))))
	PAIRS32("vmaxvq_f32", put32(vmaxvq_f32(vld1q_f32(pb))))
	PAIRS32("vminv_f32", put32(vminv_f32(vld1_f32(pa))))
	PAIRS32("vminvq_f32", put32(vminvq_f32(vld1q_f32(pb))))
	PAIRS32("vaddv_f32", put32(vaddv_f32(vld1_f32(pa))))
	PAIRS32("vaddvq_f32", put32(vaddvq_f32(vld1q_f32(pb))))

	PAIRS64("vabs_f64", float64x1_t r = vabs_f64(vld1_f64(pa)); EACH64(r, 1))
	PAIRS64("vabsq_f64", float64x2_t r = vabsq_f64(vld1q_f64(pa)); EACH64(r, 2))
	PAIRS64("vabd_f64", float64x1_t r = vabd_f64(vld1_f64(pa), vld1_f64(pb)); EACH64(r, 1))
	PAIRS64("vabdq_f64", float64x2_t r = vabdq_f64(vld1q_f64(pa), vld1q_f64(pb)); EACH64(r, 2))
	PAIRS64("vmax_f64", float64x1_t r = vmax_f64(vld1_f64(pa), vld1_f64(pb)); EACH64(r, 1))
	PAIRS64("vmaxq_f64", float64x2_t r = vmaxq_f64(vld1q_f64(pa), vld1q_f64(pb)); EACH64(r, 2))
	PAIRS64("vmin_f64", float64x1_t r = vmin_f64(vld1_f64(pa), vld1_f64(pb)); EACH64(r, 1))
	PAIRS64("vminq_f64", float64x2_t r = vminq_f64(vld1q_f64(pa), vld1q_f64(pb)); EACH64(r, 2))
	PAIRS64("vmaxvq_f64", put64(vmaxvq_f64(vld1q_f64(pa))))
	PAIRS64("vminvq_f64", put64(vminvq_f64(vld1q_f64(pa))))
	PAIRS64("vaddvq_f64", put64(vaddvq_f64(vld1q_f64(pa))))

	/* The case the issue was found with, where adding left to right rounds the other way. */
	float32_t big[4] = { single(10), single(8), single(11), single(8) };
	printf("vaddvq_f32 rounding:");
	put32(vaddvq_f32(vld1q_f32(big)));
	printf("\n");
}

static volatile uint64_t words[] = { 0x0123456789abcdef, 0xfedcba9876543210, 0, ~0ull };

#define INSERT(name, n)                                                                       \
	do {                                                                                    \
		printf("%s %d:", #name, n);                                                     \
		for (int i = 0; i < 4; i++)                                                     \
			for (int j = 0; j < 4; j++) {                                           \
				uint64_t a = words[i], b = words[j];                            \
				printf(" %016llx", (unsigned long long)vget_lane_u64(           \
					vreinterpret_u64_s64(name##_n_s64(vcreate_s64(a),       \
									 vcreate_s64(b), n)), 0)); \
				uint64x2_t r = name##q_n_u64(vdupq_n_u64(a), vdupq_n_u64(b), n); \
				printf(" %016llx", (unsigned long long)vgetq_lane_u64(r, 1));   \
				printf(" %016llx",                                               \
				       (unsigned long long)vget_lane_u64(                        \
					       name##_n_u64(vcreate_u64(a), vcreate_u64(b), n), 0)); \
			}                                                                       \
		printf("\n");                                                                   \
	} while (0)

static void inserts(void)
{
	INSERT(vsli, 0);
	INSERT(vsli, 1);
	INSERT(vsli, 32);
	INSERT(vsli, 63);
	INSERT(vsri, 1);
	INSERT(vsri, 32);
	INSERT(vsri, 63);
	INSERT(vsri, 64);
}

int main(void)
{
	floats();
	inserts();
	return 0;
}
