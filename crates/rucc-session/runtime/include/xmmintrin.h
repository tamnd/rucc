/* xmmintrin.h, the SSE intrinsics.
 *
 * `<mmintrin.h>` next door is the sixty four bit vector and the operations over it. This is the
 * one after it: a hundred and twenty eight bit vector of four floats, spelled `__m128`, and the
 * arithmetic, the comparisons, the loads and the shuffles over it. It also carries the handful of
 * operations SSE added to the older sixty four bit vector, which is why it includes that header
 * rather than standing on its own, and `<mm_malloc.h>`, because a program that loads an aligned
 * vector needs a way to allocate one.
 *
 * # These are C, not instructions
 *
 * Every function below is written in terms the compiler already understands: vector arithmetic,
 * vector comparison, a subscript that picks a lane, a brace initializer that builds one. None of
 * them is a request for a particular instruction. That makes them correct and it does not make
 * them fast, because the compiler scalarizes a vector into its lanes and works on the lanes. A
 * program gets the answers gcc gives and not the code gcc writes. Vectors that stay in registers
 * are `tamnd/rucc#200`, and when that lands these stop being the slow way round without a line of
 * this file changing.
 *
 * Writing them this way is also what makes them checkable. A shuffle written as four subscripts
 * says what it means, and the answer it produces was compared against gcc 16.2.0 lane by lane.
 *
 * # Why `static __inline__` and not what GCC writes
 *
 * gcc writes `extern __inline __attribute__((__gnu_inline__, __always_inline__))`, which asks for
 * a definition that is inlined everywhere and emitted nowhere. This compiler honours the second
 * half and not yet the first, so a call would be left behind as a reference to a symbol no object
 * file defines. `static __inline__` has the same effect for a header like this one and links.
 * `tamnd/rucc#1149` is the work that makes gcc's own spelling behave, and this file goes back to
 * it when that lands.
 *
 * # What is not here
 *
 * Six names of gcc's header are missing, and each is missing for one reason: it is an instruction
 * whose answer no amount of plain C reproduces exactly.
 *
 * `_mm_sqrt_ps` and `_mm_sqrt_ss` are a correctly rounded square root. Software can approximate
 * one but cannot cheaply be exactly right for every input, and a square root that is off by one
 * unit in the last place for some inputs is worse than one that is absent, because the first is a
 * wrong answer nobody sees and the second is a diagnostic. `_mm_rsqrt_ps` and `_mm_rsqrt_ss` are
 * the same thing behind a reciprocal. `tamnd/rucc#1157` is the square root instruction, and these
 * four arrive with it.
 *
 * `_mm_getcsr` and `_mm_setcsr` read and write the control register that holds the rounding mode
 * and the exception flags. There is no expression in C that names that register, so there is
 * nothing to write here at all. The `_MM_GET_` and `_MM_SET_` macros that gcc builds over the two
 * are absent for the same reason. The bit constants beside them are ordinary numbers and are here,
 * since a program that reads a control word it got from elsewhere still wants to name the bits.
 *
 * A program that calls one of the six gets an error naming the function it called. That is the
 * point of leaving them out rather than defining them to something close.
 *
 * `_mm_prefetch` is here and is exact, `_mm_pause` is here and does nothing, and `_mm_sfence` is
 * here as a fence one step stronger than the instruction. Each says why beside itself. */

#ifndef __RUCC_XMMINTRIN_H
#define __RUCC_XMMINTRIN_H

/* The sixty four bit vector, which several of the conversions below take or give back. */
#include <mmintrin.h>

/* `_mm_malloc` and `_mm_free`, which gcc's copy of this header includes for the same reason. */
#include <mm_malloc.h>

/* The selectors for `_mm_prefetch`. The values are gcc's and are not arbitrary: the low two bits
 * are how long to keep the line and the third bit asks for it to be written, which is what lets
 * `_mm_prefetch` below take one apart with two shifts rather than a table. */
enum _mm_hint
{
  _MM_HINT_IT0 = 19,
  _MM_HINT_IT1 = 18,
  _MM_HINT_RST2 = 9,
  _MM_HINT_ET0 = 7,
  _MM_HINT_T0 = 3,
  _MM_HINT_T1 = 2,
  _MM_HINT_T2 = 1,
  _MM_HINT_NTA = 0
};

/* The selector `_mm_shuffle_ps` takes, built from the four lane numbers in the order the answer
 * has them. Two bits each, lowest lane in the lowest bits. */
#define _MM_SHUFFLE(fp3, fp2, fp1, fp0) \
  (((fp3) << 6) | ((fp2) << 4) | ((fp1) << 2) | (fp0))

/* The bits of the control register. `_mm_getcsr` and `_mm_setcsr` are not here, as the comment at
 * the top says, but a program that has a control word from somewhere else still names its bits
 * with these, so they cost nothing to carry and are exactly gcc's values. */
#define _MM_EXCEPT_MASK 0x003f
#define _MM_EXCEPT_INVALID 0x0001
#define _MM_EXCEPT_DENORM 0x0002
#define _MM_EXCEPT_DIV_ZERO 0x0004
#define _MM_EXCEPT_OVERFLOW 0x0008
#define _MM_EXCEPT_UNDERFLOW 0x0010
#define _MM_EXCEPT_INEXACT 0x0020

#define _MM_MASK_MASK 0x1f80
#define _MM_MASK_INVALID 0x0080
#define _MM_MASK_DENORM 0x0100
#define _MM_MASK_DIV_ZERO 0x0200
#define _MM_MASK_OVERFLOW 0x0400
#define _MM_MASK_UNDERFLOW 0x0800
#define _MM_MASK_INEXACT 0x1000

#define _MM_ROUND_MASK 0x6000
#define _MM_ROUND_NEAREST 0x0000
#define _MM_ROUND_DOWN 0x2000
#define _MM_ROUND_UP 0x4000
#define _MM_ROUND_TOWARD_ZERO 0x6000

#define _MM_FLUSH_ZERO_MASK 0x8000
#define _MM_FLUSH_ZERO_ON 0x8000
#define _MM_FLUSH_ZERO_OFF 0x0000

/* The lane types. Four floats is what the register holds, and the two integer spellings are the
 * same sixteen bytes read as lanes of a width the float operations do not have: a comparison
 * produces a mask and a mask is not a number, so it is built and carried as an integer vector and
 * cast back at the end. The unsigned one is there because a shift of a mask has to bring in zeros.
 *
 * These are the sixteen byte types and the ones in `<mmintrin.h>` are the eight byte types, so
 * nothing here collides with anything there. */
typedef float __v4sf __attribute__((__vector_size__(16)));
typedef int __v4si __attribute__((__vector_size__(16)));
typedef unsigned int __v4su __attribute__((__vector_size__(16)));

/* Two floats, which is eight bytes and so is half of the above. Not a type any operation here
 * works on. It is how the half vector loads and stores name the eight bytes they move, since the
 * half of a `__m128` that those four move is two floats and `__m64` would read it as integers. */
typedef float __v2sf __attribute__((__vector_size__(8)));

/* The vector a program names. `__may_alias__` because a program is allowed to point one of these
 * at bytes it also reads as floats, which is what every load below does. */
typedef float __m128 __attribute__((__vector_size__(16), __may_alias__));

/* The same type with no alignment, which is how an unaligned load and store are written. Reading
 * through a pointer to this is reading sixteen bytes from an address that need not be aligned,
 * and that is all `_mm_loadu_ps` is.
 *
 * gcc writes the same thing as a read of a member of a struct marked `__packed__`, which this
 * compiler does not accept yet and which is `tamnd/rucc#1158`. The two produce the same answers,
 * checked at a misaligned address against gcc 16.2.0, so this is a spelling and not a compromise. */
typedef float __m128_u __attribute__((__vector_size__(16), __may_alias__, __aligned__(1)));

/* A value nobody has written to yet, which is what `_mm_undefined_ps` is for: a program building
 * a vector lane by lane starts from one of these rather than from a zero it does not need. */
static __inline__ __m128 __attribute__((__always_inline__)) _mm_undefined_ps(void)
{
  __m128 __answer;
  return __answer;
}

/* All four lanes zero. */
static __inline__ __m128 __attribute__((__always_inline__)) _mm_setzero_ps(void)
{
  return (__m128)(__v4sf){ 0.0f, 0.0f, 0.0f, 0.0f };
}

/* # Arithmetic
 *
 * Each operation comes in two forms. The `ps` form does the work on all four lanes. The `ss` form
 * does it on lane zero and takes the other three from the left operand unchanged, which is what
 * makes it a scalar operation on a vector register rather than a vector one. */

static __inline__ __m128 __attribute__((__always_inline__)) _mm_add_ps(__m128 __a, __m128 __b)
{
  return (__m128)((__v4sf)__a + (__v4sf)__b);
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_add_ss(__m128 __a, __m128 __b)
{
  __v4sf __answer = (__v4sf)__a;
  __answer[0] = __answer[0] + ((__v4sf)__b)[0];
  return (__m128)__answer;
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_sub_ps(__m128 __a, __m128 __b)
{
  return (__m128)((__v4sf)__a - (__v4sf)__b);
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_sub_ss(__m128 __a, __m128 __b)
{
  __v4sf __answer = (__v4sf)__a;
  __answer[0] = __answer[0] - ((__v4sf)__b)[0];
  return (__m128)__answer;
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_mul_ps(__m128 __a, __m128 __b)
{
  return (__m128)((__v4sf)__a * (__v4sf)__b);
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_mul_ss(__m128 __a, __m128 __b)
{
  __v4sf __answer = (__v4sf)__a;
  __answer[0] = __answer[0] * ((__v4sf)__b)[0];
  return (__m128)__answer;
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_div_ps(__m128 __a, __m128 __b)
{
  return (__m128)((__v4sf)__a / (__v4sf)__b);
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_div_ss(__m128 __a, __m128 __b)
{
  __v4sf __answer = (__v4sf)__a;
  __answer[0] = __answer[0] / ((__v4sf)__b)[0];
  return (__m128)__answer;
}

/* The reciprocal, which the instruction computes approximately and this computes exactly.
 *
 * That is allowed and is not a corner cut. Intel documents `rcpps` as having a relative error no
 * larger than one and a half times two to the minus twelve, which is a bound and not a value, and
 * an exact division is inside it by a wide margin. A program that depends on getting the same
 * wrong answer the hardware gives is depending on something the manual does not promise, and one
 * that uses this as the estimate it is named after gets a better estimate than it asked for. */
static __inline__ __m128 __attribute__((__always_inline__)) _mm_rcp_ps(__m128 __a)
{
  return (__m128)((__v4sf){ 1.0f, 1.0f, 1.0f, 1.0f } / (__v4sf)__a);
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_rcp_ss(__m128 __a)
{
  __v4sf __answer = (__v4sf)__a;
  __answer[0] = 1.0f / __answer[0];
  return (__m128)__answer;
}

/* The smaller and the larger of two vectors, lane by lane.
 *
 * Written as a conditional rather than as a call to `fmin`, because `minps` is not `fmin`. The
 * instruction answers the second operand whenever the comparison is false, which is what happens
 * when either operand is a nan and what happens when the two are zeros of opposite sign, and
 * `fmin` has the opposite rule for a nan. The conditional below has exactly the instruction's
 * rule, so `_mm_min_ps(nan, x)` is `x` and `_mm_min_ps(x, nan)` is the nan, the same both ways
 * round as on the machine. */
static __inline__ __m128 __attribute__((__always_inline__)) _mm_min_ps(__m128 __a, __m128 __b)
{
  __v4sf __x = (__v4sf)__a, __y = (__v4sf)__b;
  return (__m128)(__v4sf){ __x[0] < __y[0] ? __x[0] : __y[0], __x[1] < __y[1] ? __x[1] : __y[1],
                           __x[2] < __y[2] ? __x[2] : __y[2], __x[3] < __y[3] ? __x[3] : __y[3] };
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_min_ss(__m128 __a, __m128 __b)
{
  __v4sf __answer = (__v4sf)__a;
  float __y = ((__v4sf)__b)[0];
  __answer[0] = __answer[0] < __y ? __answer[0] : __y;
  return (__m128)__answer;
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_max_ps(__m128 __a, __m128 __b)
{
  __v4sf __x = (__v4sf)__a, __y = (__v4sf)__b;
  return (__m128)(__v4sf){ __x[0] > __y[0] ? __x[0] : __y[0], __x[1] > __y[1] ? __x[1] : __y[1],
                           __x[2] > __y[2] ? __x[2] : __y[2], __x[3] > __y[3] ? __x[3] : __y[3] };
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_max_ss(__m128 __a, __m128 __b)
{
  __v4sf __answer = (__v4sf)__a;
  float __y = ((__v4sf)__b)[0];
  __answer[0] = __answer[0] > __y ? __answer[0] : __y;
  return (__m128)__answer;
}

/* # The bitwise operations
 *
 * There is no `&` on a vector of floats, so each is done on the same bytes read as integers and
 * cast back. That is what the instruction does too: `andps` has no idea it is holding floats. */

static __inline__ __m128 __attribute__((__always_inline__)) _mm_and_ps(__m128 __a, __m128 __b)
{
  return (__m128)((__v4su)__a & (__v4su)__b);
}

/* The complement of the first and the second, in that order. The name says `andnot` and the
 * operand that is complemented is the first one, which is the opposite of how it reads. */
static __inline__ __m128 __attribute__((__always_inline__)) _mm_andnot_ps(__m128 __a, __m128 __b)
{
  return (__m128)(~(__v4su)__a & (__v4su)__b);
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_or_ps(__m128 __a, __m128 __b)
{
  return (__m128)((__v4su)__a | (__v4su)__b);
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_xor_ps(__m128 __a, __m128 __b)
{
  return (__m128)((__v4su)__a ^ (__v4su)__b);
}

/* # The comparisons
 *
 * Each answers a mask and not a truth value: every bit of a lane is set where the comparison held
 * and clear where it did not, which is what makes the answer something a program can hand to
 * `_mm_and_ps` and use to select. C's vector comparison produces exactly that, so the `ps` forms
 * are the operator and a cast.
 *
 * The six spelled with an `n` are the complement of the six without it, and a complement is not
 * the opposite comparison: `cmpnlt` holds when `a < b` is false, which includes every pair with a
 * nan in it, while `cmpge` does not. Writing them as `~` over the other mask is what keeps that
 * right without thinking about it.
 *
 * `cmpord` holds where neither operand is a nan, which is written as each being equal to itself,
 * and `cmpunord` is its complement. */

static __inline__ __m128 __attribute__((__always_inline__)) _mm_cmpeq_ps(__m128 __a, __m128 __b)
{
  return (__m128)((__v4si)((__v4sf)__a == (__v4sf)__b));
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_cmplt_ps(__m128 __a, __m128 __b)
{
  return (__m128)((__v4si)((__v4sf)__a < (__v4sf)__b));
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_cmple_ps(__m128 __a, __m128 __b)
{
  return (__m128)((__v4si)((__v4sf)__a <= (__v4sf)__b));
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_cmpgt_ps(__m128 __a, __m128 __b)
{
  return (__m128)((__v4si)((__v4sf)__a > (__v4sf)__b));
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_cmpge_ps(__m128 __a, __m128 __b)
{
  return (__m128)((__v4si)((__v4sf)__a >= (__v4sf)__b));
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_cmpneq_ps(__m128 __a, __m128 __b)
{
  return (__m128)((__v4si)((__v4sf)__a != (__v4sf)__b));
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_cmpnlt_ps(__m128 __a, __m128 __b)
{
  return (__m128)(~(__v4si)((__v4sf)__a < (__v4sf)__b));
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_cmpnle_ps(__m128 __a, __m128 __b)
{
  return (__m128)(~(__v4si)((__v4sf)__a <= (__v4sf)__b));
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_cmpngt_ps(__m128 __a, __m128 __b)
{
  return (__m128)(~(__v4si)((__v4sf)__a > (__v4sf)__b));
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_cmpnge_ps(__m128 __a, __m128 __b)
{
  return (__m128)(~(__v4si)((__v4sf)__a >= (__v4sf)__b));
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_cmpord_ps(__m128 __a, __m128 __b)
{
  return (__m128)((__v4si)((__v4sf)__a == (__v4sf)__a) & (__v4si)((__v4sf)__b == (__v4sf)__b));
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_cmpunord_ps(__m128 __a, __m128 __b)
{
  return (__m128)(~((__v4si)((__v4sf)__a == (__v4sf)__a) & (__v4si)((__v4sf)__b == (__v4sf)__b)));
}

/* The `ss` comparisons, which put the mask in lane zero and leave the other three lanes as they
 * were in the left operand. Those three lanes are floats and lane zero is now a mask, so the
 * value is assembled through the unsigned integer spelling and cast back once at the end. */

static __inline__ __m128 __attribute__((__always_inline__)) _mm_cmpeq_ss(__m128 __a, __m128 __b)
{
  __v4su __answer = (__v4su)__a;
  __answer[0] = (unsigned int)((__v4si)((__v4sf)__a == (__v4sf)__b))[0];
  return (__m128)__answer;
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_cmplt_ss(__m128 __a, __m128 __b)
{
  __v4su __answer = (__v4su)__a;
  __answer[0] = (unsigned int)((__v4si)((__v4sf)__a < (__v4sf)__b))[0];
  return (__m128)__answer;
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_cmple_ss(__m128 __a, __m128 __b)
{
  __v4su __answer = (__v4su)__a;
  __answer[0] = (unsigned int)((__v4si)((__v4sf)__a <= (__v4sf)__b))[0];
  return (__m128)__answer;
}

/* `cmpgtss` and `cmpgess` are not instructions. The machine has the two less than forms and gets
 * the greater than ones by swapping the operands, and so does this, which matters because the
 * answer keeps the upper lanes of whichever operand is on the left. gcc swaps the same way. */
static __inline__ __m128 __attribute__((__always_inline__)) _mm_cmpgt_ss(__m128 __a, __m128 __b)
{
  __v4su __answer = (__v4su)__a;
  __answer[0] = (unsigned int)((__v4si)((__v4sf)__a > (__v4sf)__b))[0];
  return (__m128)__answer;
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_cmpge_ss(__m128 __a, __m128 __b)
{
  __v4su __answer = (__v4su)__a;
  __answer[0] = (unsigned int)((__v4si)((__v4sf)__a >= (__v4sf)__b))[0];
  return (__m128)__answer;
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_cmpneq_ss(__m128 __a, __m128 __b)
{
  __v4su __answer = (__v4su)__a;
  __answer[0] = (unsigned int)((__v4si)((__v4sf)__a != (__v4sf)__b))[0];
  return (__m128)__answer;
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_cmpnlt_ss(__m128 __a, __m128 __b)
{
  __v4su __answer = (__v4su)__a;
  __answer[0] = (unsigned int)(~(__v4si)((__v4sf)__a < (__v4sf)__b))[0];
  return (__m128)__answer;
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_cmpnle_ss(__m128 __a, __m128 __b)
{
  __v4su __answer = (__v4su)__a;
  __answer[0] = (unsigned int)(~(__v4si)((__v4sf)__a <= (__v4sf)__b))[0];
  return (__m128)__answer;
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_cmpngt_ss(__m128 __a, __m128 __b)
{
  __v4su __answer = (__v4su)__a;
  __answer[0] = (unsigned int)(~(__v4si)((__v4sf)__a > (__v4sf)__b))[0];
  return (__m128)__answer;
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_cmpnge_ss(__m128 __a, __m128 __b)
{
  __v4su __answer = (__v4su)__a;
  __answer[0] = (unsigned int)(~(__v4si)((__v4sf)__a >= (__v4sf)__b))[0];
  return (__m128)__answer;
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_cmpord_ss(__m128 __a, __m128 __b)
{
  __v4su __answer = (__v4su)__a;
  float __x = ((__v4sf)__a)[0], __y = ((__v4sf)__b)[0];
  __answer[0] = __x == __x && __y == __y ? 0xffffffffu : 0u;
  return (__m128)__answer;
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_cmpunord_ss(__m128 __a, __m128 __b)
{
  __v4su __answer = (__v4su)__a;
  float __x = ((__v4sf)__a)[0], __y = ((__v4sf)__b)[0];
  __answer[0] = __x == __x && __y == __y ? 0u : 0xffffffffu;
  return (__m128)__answer;
}

/* # The comparisons that answer an int
 *
 * These compare lane zero and give back nought or one rather than a mask. The `comi` forms and
 * the `ucomi` forms have the same answer for every pair of inputs, and differ only in that the
 * first raises the invalid exception for a quiet nan and the second does not. Nothing here reads
 * the exception flags, since `_mm_getcsr` is not here, so the two are written once each with the
 * same body and the pair remains honest about the value it produces. */

static __inline__ int __attribute__((__always_inline__)) _mm_comieq_ss(__m128 __a, __m128 __b)
{
  return ((__v4sf)__a)[0] == ((__v4sf)__b)[0];
}

static __inline__ int __attribute__((__always_inline__)) _mm_comilt_ss(__m128 __a, __m128 __b)
{
  return ((__v4sf)__a)[0] < ((__v4sf)__b)[0];
}

static __inline__ int __attribute__((__always_inline__)) _mm_comile_ss(__m128 __a, __m128 __b)
{
  return ((__v4sf)__a)[0] <= ((__v4sf)__b)[0];
}

static __inline__ int __attribute__((__always_inline__)) _mm_comigt_ss(__m128 __a, __m128 __b)
{
  return ((__v4sf)__a)[0] > ((__v4sf)__b)[0];
}

static __inline__ int __attribute__((__always_inline__)) _mm_comige_ss(__m128 __a, __m128 __b)
{
  return ((__v4sf)__a)[0] >= ((__v4sf)__b)[0];
}

/* True when the two are unequal, which a nan on either side makes true. */
static __inline__ int __attribute__((__always_inline__)) _mm_comineq_ss(__m128 __a, __m128 __b)
{
  return ((__v4sf)__a)[0] != ((__v4sf)__b)[0];
}

static __inline__ int __attribute__((__always_inline__)) _mm_ucomieq_ss(__m128 __a, __m128 __b)
{
  return ((__v4sf)__a)[0] == ((__v4sf)__b)[0];
}

static __inline__ int __attribute__((__always_inline__)) _mm_ucomilt_ss(__m128 __a, __m128 __b)
{
  return ((__v4sf)__a)[0] < ((__v4sf)__b)[0];
}

static __inline__ int __attribute__((__always_inline__)) _mm_ucomile_ss(__m128 __a, __m128 __b)
{
  return ((__v4sf)__a)[0] <= ((__v4sf)__b)[0];
}

static __inline__ int __attribute__((__always_inline__)) _mm_ucomigt_ss(__m128 __a, __m128 __b)
{
  return ((__v4sf)__a)[0] > ((__v4sf)__b)[0];
}

static __inline__ int __attribute__((__always_inline__)) _mm_ucomige_ss(__m128 __a, __m128 __b)
{
  return ((__v4sf)__a)[0] >= ((__v4sf)__b)[0];
}

static __inline__ int __attribute__((__always_inline__)) _mm_ucomineq_ss(__m128 __a, __m128 __b)
{
  return ((__v4sf)__a)[0] != ((__v4sf)__b)[0];
}

/* # The comparison that takes the predicate as a number
 *
 * `cmpps` carries the comparison in an immediate, and these two are how a program names it. The
 * eight predicates below are the eight SSE has. AVX widened the immediate and added twenty four
 * more, which need the wider instruction.
 *
 * Two differences from gcc, both in the direction of accepting more, and both measured against
 * gcc 16.2.0. gcc requires the predicate to be a constant the compiler can see, since it ends up
 * in an instruction, and refuses one worked out at run time; this is an ordinary function and
 * takes either. gcc refuses a predicate of eight or above outright, asking for `-mavx`; this
 * answers a mask of zeros for those rather than refusing, because a header cannot refuse. Every
 * program that compiles under gcc gets the same answers here, which is what matters, and the
 * extra cases are ones gcc would not have built at all. */
static __inline__ __m128 __attribute__((__always_inline__))
_mm_cmp_ps(__m128 __a, __m128 __b, const int __predicate)
{
  switch (__predicate & 31)
  {
    case 0:
      return _mm_cmpeq_ps(__a, __b);
    case 1:
      return _mm_cmplt_ps(__a, __b);
    case 2:
      return _mm_cmple_ps(__a, __b);
    case 3:
      return _mm_cmpunord_ps(__a, __b);
    case 4:
      return _mm_cmpneq_ps(__a, __b);
    case 5:
      return _mm_cmpnlt_ps(__a, __b);
    case 6:
      return _mm_cmpnle_ps(__a, __b);
    case 7:
      return _mm_cmpord_ps(__a, __b);
    default:
      return _mm_setzero_ps();
  }
}

static __inline__ __m128 __attribute__((__always_inline__))
_mm_cmp_ss(__m128 __a, __m128 __b, const int __predicate)
{
  switch (__predicate & 31)
  {
    case 0:
      return _mm_cmpeq_ss(__a, __b);
    case 1:
      return _mm_cmplt_ss(__a, __b);
    case 2:
      return _mm_cmple_ss(__a, __b);
    case 3:
      return _mm_cmpunord_ss(__a, __b);
    case 4:
      return _mm_cmpneq_ss(__a, __b);
    case 5:
      return _mm_cmpnlt_ss(__a, __b);
    case 6:
      return _mm_cmpnle_ss(__a, __b);
    case 7:
      return _mm_cmpord_ss(__a, __b);
    default:
    {
      __v4su __answer = (__v4su)__a;
      __answer[0] = 0u;
      return (__m128)__answer;
    }
  }
}

/* # Rounding
 *
 * `cvtss2si` and the conversions beside it take a float to an integer under whatever rounding
 * mode the program is running in, where a cast in C always truncates. The two differ by default,
 * since the default mode is to nearest with ties going to the even answer, so a cast is the wrong
 * answer for `_mm_cvtss_si32` and the right one for `_mm_cvttss_si32`.
 *
 * The helper below is the standard way to round a float to an integral float using nothing but
 * float addition. Adding two to the twenty third and taking it away again forces every bit below
 * the point off the end of the significand, and the rounding that happens when it goes off the
 * end is done by the hardware under the mode the program set, which is exactly what is wanted:
 * this follows the rounding mode without being able to read it. Values already that large are
 * integers and are returned as they are, which is also what keeps infinities and nans alone. The
 * sign is taken off first and put back after so that a negative value that rounds to zero gives
 * a negative zero, which is what the instruction gives. */
static __inline__ float __attribute__((__always_inline__)) __rucc_round_to_integral(float __x)
{
  float __magic = 8388608.0f;
  float __size = __x < 0.0f ? -__x : __x;
  float __answer;
  if (!(__size < __magic))
    return __x;
  __answer = (__size + __magic) - __magic;
  return __x < 0.0f ? -__answer : __answer;
}

/* A rounded float as an int, with the answer the machine gives when it does not fit.
 *
 * `cvtss2si` produces the integer indefinite value, which is the most negative int, for an input
 * that is a nan or an infinity or simply too large. A cast in C is undefined behaviour in those
 * cases, so the range is tested first and the machine's answer given rather than the cast's. */
static __inline__ int __attribute__((__always_inline__)) __rucc_float_to_int(float __x)
{
  float __rounded = __rucc_round_to_integral(__x);
  if (!(__rounded >= -2147483648.0f) || !(__rounded < 2147483648.0f))
    return (-2147483647 - 1);
  return (int)__rounded;
}

static __inline__ long long __attribute__((__always_inline__)) __rucc_float_to_long(float __x)
{
  float __rounded = __rucc_round_to_integral(__x);
  if (!(__rounded >= -9223372036854775808.0f) || !(__rounded < 9223372036854775808.0f))
    return (-9223372036854775807LL - 1);
  return (long long)__rounded;
}

/* The same two for the truncating forms, where the rounding is the cast's own and only the range
 * has to be looked after. */
static __inline__ int __attribute__((__always_inline__)) __rucc_float_to_int_trunc(float __x)
{
  if (!(__x > -2147483649.0f) || !(__x < 2147483648.0f))
    return (-2147483647 - 1);
  return (int)__x;
}

static __inline__ long long __attribute__((__always_inline__)) __rucc_float_to_long_trunc(float __x)
{
  if (!(__x > -9223372036854777856.0f) || !(__x < 9223372036854775808.0f))
    return (-9223372036854775807LL - 1);
  return (long long)__x;
}

/* # Conversions between a float vector and a scalar */

/* Lane zero as an ordinary float, which is a read and not a conversion. */
static __inline__ float __attribute__((__always_inline__)) _mm_cvtss_f32(__m128 __a)
{
  return ((__v4sf)__a)[0];
}

static __inline__ int __attribute__((__always_inline__)) _mm_cvtss_si32(__m128 __a)
{
  return __rucc_float_to_int(((__v4sf)__a)[0]);
}

static __inline__ int __attribute__((__always_inline__)) _mm_cvttss_si32(__m128 __a)
{
  return __rucc_float_to_int_trunc(((__v4sf)__a)[0]);
}

/* An int into lane zero, the other three lanes untouched. */
static __inline__ __m128 __attribute__((__always_inline__)) _mm_cvtsi32_ss(__m128 __a, int __b)
{
  __v4sf __answer = (__v4sf)__a;
  __answer[0] = (float)__b;
  return (__m128)__answer;
}

#ifdef __x86_64__
static __inline__ long long __attribute__((__always_inline__)) _mm_cvtss_si64(__m128 __a)
{
  return __rucc_float_to_long(((__v4sf)__a)[0]);
}

static __inline__ long long __attribute__((__always_inline__)) _mm_cvttss_si64(__m128 __a)
{
  return __rucc_float_to_long_trunc(((__v4sf)__a)[0]);
}

static __inline__ __m128 __attribute__((__always_inline__))
_mm_cvtsi64_ss(__m128 __a, long long __b)
{
  __v4sf __answer = (__v4sf)__a;
  __answer[0] = (float)__b;
  return (__m128)__answer;
}

/* The `x` spellings are the same three functions under the names an older compiler used. */
static __inline__ long long __attribute__((__always_inline__)) _mm_cvtss_si64x(__m128 __a)
{
  return __rucc_float_to_long(((__v4sf)__a)[0]);
}

static __inline__ long long __attribute__((__always_inline__)) _mm_cvttss_si64x(__m128 __a)
{
  return __rucc_float_to_long_trunc(((__v4sf)__a)[0]);
}

static __inline__ __m128 __attribute__((__always_inline__))
_mm_cvtsi64x_ss(__m128 __a, long long __b)
{
  __v4sf __answer = (__v4sf)__a;
  __answer[0] = (float)__b;
  return (__m128)__answer;
}
#endif

/* # Conversions between a float vector and the sixty four bit vector
 *
 * These are the operations SSE added to MMX. Each one reads the eight byte vector as lanes of the
 * width its name says and converts them. */

/* Two ints into the low two lanes, the high two taken from the first operand. */
static __inline__ __m128 __attribute__((__always_inline__)) _mm_cvtpi32_ps(__m128 __a, __m64 __b)
{
  __v4sf __answer = (__v4sf)__a;
  __v2si __from = (__v2si)__b;
  __answer[0] = (float)__from[0];
  __answer[1] = (float)__from[1];
  return (__m128)__answer;
}

/* Four ints, two from each operand, filling all four lanes. */
static __inline__ __m128 __attribute__((__always_inline__)) _mm_cvtpi32x2_ps(__m64 __a, __m64 __b)
{
  __v2si __low = (__v2si)__a, __high = (__v2si)__b;
  return (__m128)(__v4sf){ (float)__low[0], (float)__low[1], (float)__high[0], (float)__high[1] };
}

/* Four signed shorts. */
static __inline__ __m128 __attribute__((__always_inline__)) _mm_cvtpi16_ps(__m64 __a)
{
  __v4hi __from = (__v4hi)__a;
  return (__m128)(__v4sf){ (float)__from[0], (float)__from[1], (float)__from[2],
                           (float)__from[3] };
}

/* Four unsigned shorts. */
static __inline__ __m128 __attribute__((__always_inline__)) _mm_cvtpu16_ps(__m64 __a)
{
  __v4hu __from = (__v4hu)__a;
  return (__m128)(__v4sf){ (float)__from[0], (float)__from[1], (float)__from[2],
                           (float)__from[3] };
}

/* The low four of eight signed bytes. The high four are not part of the answer. */
static __inline__ __m128 __attribute__((__always_inline__)) _mm_cvtpi8_ps(__m64 __a)
{
  __v8qi __from = (__v8qi)__a;
  return (__m128)(__v4sf){ (float)__from[0], (float)__from[1], (float)__from[2],
                           (float)__from[3] };
}

/* The low four of eight unsigned bytes. */
static __inline__ __m128 __attribute__((__always_inline__)) _mm_cvtpu8_ps(__m64 __a)
{
  __v8qu __from = (__v8qu)__a;
  return (__m128)(__v4sf){ (float)__from[0], (float)__from[1], (float)__from[2],
                           (float)__from[3] };
}

/* The low two lanes as two ints, under the current rounding mode. */
static __inline__ __m64 __attribute__((__always_inline__)) _mm_cvtps_pi32(__m128 __a)
{
  __v4sf __from = (__v4sf)__a;
  return (__m64)(__v2si){ __rucc_float_to_int(__from[0]), __rucc_float_to_int(__from[1]) };
}

/* The same two, truncated. */
static __inline__ __m64 __attribute__((__always_inline__)) _mm_cvttps_pi32(__m128 __a)
{
  __v4sf __from = (__v4sf)__a;
  return (__m64)(__v2si){ __rucc_float_to_int_trunc(__from[0]),
                          __rucc_float_to_int_trunc(__from[1]) };
}

/* All four lanes as four shorts. Each float becomes an int the way `_mm_cvtps_pi32` makes one and
 * the four ints are then packed with saturation, which is how the instruction pair gcc emits for
 * this behaves and is why a large float comes out as the largest short rather than wrapping. */
static __inline__ __m64 __attribute__((__always_inline__)) _mm_cvtps_pi16(__m128 __a)
{
  __v4sf __from = (__v4sf)__a;
  return (__m64)(__v4hi){ __rucc_sat_hi(__rucc_float_to_int(__from[0])),
                          __rucc_sat_hi(__rucc_float_to_int(__from[1])),
                          __rucc_sat_hi(__rucc_float_to_int(__from[2])),
                          __rucc_sat_hi(__rucc_float_to_int(__from[3])) };
}

/* All four lanes as four bytes in the low half, the high half zero. Saturating twice, first to a
 * short and then to a byte, which is the pair of pack instructions this stands for. */
static __inline__ __m64 __attribute__((__always_inline__)) _mm_cvtps_pi8(__m128 __a)
{
  __v4sf __from = (__v4sf)__a;
  return (__m64)(__v8qi){ __rucc_sat_qi(__rucc_sat_hi(__rucc_float_to_int(__from[0]))),
                          __rucc_sat_qi(__rucc_sat_hi(__rucc_float_to_int(__from[1]))),
                          __rucc_sat_qi(__rucc_sat_hi(__rucc_float_to_int(__from[2]))),
                          __rucc_sat_qi(__rucc_sat_hi(__rucc_float_to_int(__from[3]))),
                          0, 0, 0, 0 };
}

/* # Building a vector out of floats */

/* The four arguments highest lane first, which is the order the name is written in and the
 * opposite of the order the lanes are in. `_mm_setr_ps` below is the same thing the other way
 * round, and the `r` is for reversed. */
static __inline__ __m128 __attribute__((__always_inline__))
_mm_set_ps(const float __z, const float __y, const float __x, const float __w)
{
  return (__m128)(__v4sf){ __w, __x, __y, __z };
}

static __inline__ __m128 __attribute__((__always_inline__))
_mm_setr_ps(float __z, float __y, float __x, float __w)
{
  return (__m128)(__v4sf){ __z, __y, __x, __w };
}

/* One float in lane zero and zeros above it. */
static __inline__ __m128 __attribute__((__always_inline__)) _mm_set_ss(float __f)
{
  return (__m128)(__v4sf){ __f, 0.0f, 0.0f, 0.0f };
}

/* One float in all four lanes. */
static __inline__ __m128 __attribute__((__always_inline__)) _mm_set1_ps(float __f)
{
  return (__m128)(__v4sf){ __f, __f, __f, __f };
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_set_ps1(float __f)
{
  return _mm_set1_ps(__f);
}

/* # Loads and stores
 *
 * The aligned forms read and write through a pointer to `__m128`, whose alignment is sixteen, and
 * the unaligned forms go through `__m128_u`, whose alignment is one. A program that hands an
 * unaligned address to an aligned form is doing something the machine faults on, and the type is
 * what says which is which. */

static __inline__ __m128 __attribute__((__always_inline__)) _mm_load_ps(const float *__p)
{
  return *(const __m128 *)__p;
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_loadu_ps(const float *__p)
{
  return *(const __m128_u *)__p;
}

/* One float into lane zero, zeros above it. */
static __inline__ __m128 __attribute__((__always_inline__)) _mm_load_ss(const float *__p)
{
  return _mm_set_ss(*__p);
}

/* One float into all four lanes. */
static __inline__ __m128 __attribute__((__always_inline__)) _mm_load1_ps(const float *__p)
{
  return _mm_set1_ps(*__p);
}

static __inline__ __m128 __attribute__((__always_inline__)) _mm_load_ps1(const float *__p)
{
  return _mm_load1_ps(__p);
}

/* Four floats in the opposite order to the one they are in memory. */
static __inline__ __m128 __attribute__((__always_inline__)) _mm_loadr_ps(const float *__p)
{
  __v4sf __from = (__v4sf)*(const __m128 *)__p;
  return (__m128)(__v4sf){ __from[3], __from[2], __from[1], __from[0] };
}

/* Two floats from memory into the upper half, the lower half kept. The pointer is to the eight
 * byte vector because that is the width being moved, and the eight bytes are two floats. */
static __inline__ __m128 __attribute__((__always_inline__))
_mm_loadh_pi(__m128 __a, __m64 const *__p)
{
  __v4sf __answer = (__v4sf)__a;
  __v2sf __from = *(const __v2sf *)__p;
  __answer[2] = __from[0];
  __answer[3] = __from[1];
  return (__m128)__answer;
}

/* Two floats from memory into the lower half, the upper half kept. */
static __inline__ __m128 __attribute__((__always_inline__))
_mm_loadl_pi(__m128 __a, __m64 const *__p)
{
  __v4sf __answer = (__v4sf)__a;
  __v2sf __from = *(const __v2sf *)__p;
  __answer[0] = __from[0];
  __answer[1] = __from[1];
  return (__m128)__answer;
}

static __inline__ void __attribute__((__always_inline__)) _mm_store_ps(float *__p, __m128 __a)
{
  *(__m128 *)__p = __a;
}

static __inline__ void __attribute__((__always_inline__)) _mm_storeu_ps(float *__p, __m128 __a)
{
  *(__m128_u *)__p = __a;
}

/* Lane zero alone, which is four bytes and not sixteen. */
static __inline__ void __attribute__((__always_inline__)) _mm_store_ss(float *__p, __m128 __a)
{
  *__p = ((__v4sf)__a)[0];
}

/* Lane zero written to all four places. */
static __inline__ void __attribute__((__always_inline__)) _mm_store1_ps(float *__p, __m128 __a)
{
  *(__m128 *)__p = _mm_set1_ps(((__v4sf)__a)[0]);
}

static __inline__ void __attribute__((__always_inline__)) _mm_store_ps1(float *__p, __m128 __a)
{
  _mm_store1_ps(__p, __a);
}

/* Four floats in the opposite order to the one they are in the vector. */
static __inline__ void __attribute__((__always_inline__)) _mm_storer_ps(float *__p, __m128 __a)
{
  __v4sf __from = (__v4sf)__a;
  *(__m128 *)__p = (__m128)(__v4sf){ __from[3], __from[2], __from[1], __from[0] };
}

static __inline__ void __attribute__((__always_inline__)) _mm_storeh_pi(__m64 *__p, __m128 __a)
{
  __v4sf __from = (__v4sf)__a;
  *(__v2sf *)__p = (__v2sf){ __from[2], __from[3] };
}

static __inline__ void __attribute__((__always_inline__)) _mm_storel_pi(__m64 *__p, __m128 __a)
{
  __v4sf __from = (__v4sf)__a;
  *(__v2sf *)__p = (__v2sf){ __from[0], __from[1] };
}

/* # Moving lanes about */

/* Lane zero from the second operand and the rest from the first. */
static __inline__ __m128 __attribute__((__always_inline__)) _mm_move_ss(__m128 __a, __m128 __b)
{
  __v4sf __answer = (__v4sf)__a;
  __answer[0] = ((__v4sf)__b)[0];
  return (__m128)__answer;
}

/* The upper half of the second operand into the lower half of the answer, the upper half of the
 * first operand left where it is. The operands read backwards for the same reason the instruction
 * is named the way it is: it moves high to low. */
static __inline__ __m128 __attribute__((__always_inline__)) _mm_movehl_ps(__m128 __a, __m128 __b)
{
  __v4sf __x = (__v4sf)__a, __y = (__v4sf)__b;
  return (__m128)(__v4sf){ __y[2], __y[3], __x[2], __x[3] };
}

/* The lower half of the second operand into the upper half of the answer. */
static __inline__ __m128 __attribute__((__always_inline__)) _mm_movelh_ps(__m128 __a, __m128 __b)
{
  __v4sf __x = (__v4sf)__a, __y = (__v4sf)__b;
  return (__m128)(__v4sf){ __x[0], __x[1], __y[0], __y[1] };
}

/* The two upper lanes of each operand, interleaved. */
static __inline__ __m128 __attribute__((__always_inline__)) _mm_unpackhi_ps(__m128 __a, __m128 __b)
{
  __v4sf __x = (__v4sf)__a, __y = (__v4sf)__b;
  return (__m128)(__v4sf){ __x[2], __y[2], __x[3], __y[3] };
}

/* The two lower lanes of each operand, interleaved. */
static __inline__ __m128 __attribute__((__always_inline__)) _mm_unpacklo_ps(__m128 __a, __m128 __b)
{
  __v4sf __x = (__v4sf)__a, __y = (__v4sf)__b;
  return (__m128)(__v4sf){ __x[0], __y[0], __x[1], __y[1] };
}

/* Four lanes chosen by a selector, the low two from the first operand and the high two from the
 * second, two bits each. `_MM_SHUFFLE` above is how a program writes the selector.
 *
 * A function rather than a macro, which gcc's header is only under optimization. A subscript into
 * a vector does not need a constant index, so the selector may be worked out at run time, and
 * writing it this way evaluates each operand once where a macro would evaluate them four times. */
static __inline__ __m128 __attribute__((__always_inline__))
_mm_shuffle_ps(__m128 __a, __m128 __b, const int __mask)
{
  __v4sf __x = (__v4sf)__a, __y = (__v4sf)__b;
  return (__m128)(__v4sf){ __x[__mask & 3], __x[(__mask >> 2) & 3], __y[(__mask >> 4) & 3],
                           __y[(__mask >> 6) & 3] };
}

/* The sign bit of each lane, lane zero in the lowest bit. */
static __inline__ int __attribute__((__always_inline__)) _mm_movemask_ps(__m128 __a)
{
  __v4su __from = (__v4su)__a;
  return (int)((__from[0] >> 31) | ((__from[1] >> 31) << 1) | ((__from[2] >> 31) << 2)
               | ((__from[3] >> 31) << 3));
}

/* Four vectors transposed in place, which is four rows of a matrix becoming four columns. A macro
 * because it writes back through its arguments, and wrapped in a loop that runs once so that it
 * is a statement and takes a semicolon like one. */
#define _MM_TRANSPOSE4_PS(row0, row1, row2, row3)                                \
  do                                                                             \
  {                                                                              \
    __v4sf __r0 = (__v4sf)(row0), __r1 = (__v4sf)(row1);                         \
    __v4sf __r2 = (__v4sf)(row2), __r3 = (__v4sf)(row3);                         \
    (row0) = (__m128)(__v4sf){ __r0[0], __r1[0], __r2[0], __r3[0] };             \
    (row1) = (__m128)(__v4sf){ __r0[1], __r1[1], __r2[1], __r3[1] };             \
    (row2) = (__m128)(__v4sf){ __r0[2], __r1[2], __r2[2], __r3[2] };             \
    (row3) = (__m128)(__v4sf){ __r0[3], __r1[3], __r2[3], __r3[3] };             \
  } while (0)

/* # What SSE added to the sixty four bit vector
 *
 * These take and give back `__m64` and are here rather than in `<mmintrin.h>` because they came
 * with SSE rather than with MMX. A program written against MMX alone never had them. */

/* The larger and the smaller of each lane, signed shorts and unsigned bytes, which are the two
 * widths the instructions cover. */
static __inline__ __m64 __attribute__((__always_inline__)) _mm_max_pi16(__m64 __a, __m64 __b)
{
  __v4hi __x = (__v4hi)__a, __y = (__v4hi)__b;
  return (__m64)(__v4hi){ __x[0] > __y[0] ? __x[0] : __y[0], __x[1] > __y[1] ? __x[1] : __y[1],
                          __x[2] > __y[2] ? __x[2] : __y[2], __x[3] > __y[3] ? __x[3] : __y[3] };
}

static __inline__ __m64 __attribute__((__always_inline__)) _mm_min_pi16(__m64 __a, __m64 __b)
{
  __v4hi __x = (__v4hi)__a, __y = (__v4hi)__b;
  return (__m64)(__v4hi){ __x[0] < __y[0] ? __x[0] : __y[0], __x[1] < __y[1] ? __x[1] : __y[1],
                          __x[2] < __y[2] ? __x[2] : __y[2], __x[3] < __y[3] ? __x[3] : __y[3] };
}

static __inline__ __m64 __attribute__((__always_inline__)) _mm_max_pu8(__m64 __a, __m64 __b)
{
  __v8qu __x = (__v8qu)__a, __y = (__v8qu)__b;
  int __i;
  __v8qu __answer;
  for (__i = 0; __i < 8; __i++)
    __answer[__i] = __x[__i] > __y[__i] ? __x[__i] : __y[__i];
  return (__m64)__answer;
}

static __inline__ __m64 __attribute__((__always_inline__)) _mm_min_pu8(__m64 __a, __m64 __b)
{
  __v8qu __x = (__v8qu)__a, __y = (__v8qu)__b;
  int __i;
  __v8qu __answer;
  for (__i = 0; __i < 8; __i++)
    __answer[__i] = __x[__i] < __y[__i] ? __x[__i] : __y[__i];
  return (__m64)__answer;
}

/* The rounded average of each lane, which is the sum plus one halved. Summed in `unsigned` so
 * that the sum of two bytes near the top of the range does not wrap before it is halved. */
static __inline__ __m64 __attribute__((__always_inline__)) _mm_avg_pu8(__m64 __a, __m64 __b)
{
  __v8qu __x = (__v8qu)__a, __y = (__v8qu)__b;
  int __i;
  __v8qu __answer;
  for (__i = 0; __i < 8; __i++)
    __answer[__i] = (unsigned char)(((unsigned int)__x[__i] + (unsigned int)__y[__i] + 1u) >> 1);
  return (__m64)__answer;
}

static __inline__ __m64 __attribute__((__always_inline__)) _mm_avg_pu16(__m64 __a, __m64 __b)
{
  __v4hu __x = (__v4hu)__a, __y = (__v4hu)__b;
  int __i;
  __v4hu __answer;
  for (__i = 0; __i < 4; __i++)
    __answer[__i] = (unsigned short)(((unsigned int)__x[__i] + (unsigned int)__y[__i] + 1u) >> 1);
  return (__m64)__answer;
}

/* The upper sixteen bits of the unsigned product of each pair of shorts. The signed form of this
 * is `_mm_mulhi_pi16` in `<mmintrin.h>`; the difference is only how the operands are read. */
static __inline__ __m64 __attribute__((__always_inline__)) _mm_mulhi_pu16(__m64 __a, __m64 __b)
{
  __v4hu __x = (__v4hu)__a, __y = (__v4hu)__b;
  int __i;
  __v4hu __answer;
  for (__i = 0; __i < 4; __i++)
    __answer[__i] = (unsigned short)(((unsigned int)__x[__i] * (unsigned int)__y[__i]) >> 16);
  return (__m64)__answer;
}

/* The sum of the absolute differences of eight bytes, as a single number in the lowest short of
 * the answer with the rest zero. The differences are taken in `int` so that the subtraction of
 * two unsigned bytes cannot wrap round to a large positive number. */
static __inline__ __m64 __attribute__((__always_inline__)) _mm_sad_pu8(__m64 __a, __m64 __b)
{
  __v8qu __x = (__v8qu)__a, __y = (__v8qu)__b;
  int __i, __total = 0;
  for (__i = 0; __i < 8; __i++)
  {
    int __difference = (int)__x[__i] - (int)__y[__i];
    __total += __difference < 0 ? -__difference : __difference;
  }
  return (__m64)(__v4hi){ (short)__total, 0, 0, 0 };
}

/* The sign bit of each of eight bytes, lowest byte in the lowest bit. */
static __inline__ int __attribute__((__always_inline__)) _mm_movemask_pi8(__m64 __a)
{
  __v8qu __from = (__v8qu)__a;
  int __i, __answer = 0;
  for (__i = 0; __i < 8; __i++)
    __answer |= (int)(__from[__i] >> 7) << __i;
  return __answer;
}

/* Four shorts chosen by a selector, two bits each, all four from the one operand. */
static __inline__ __m64 __attribute__((__always_inline__))
_mm_shuffle_pi16(__m64 __a, const int __mask)
{
  __v4hi __from = (__v4hi)__a;
  return (__m64)(__v4hi){ __from[__mask & 3], __from[(__mask >> 2) & 3],
                          __from[(__mask >> 4) & 3], __from[(__mask >> 6) & 3] };
}

/* One short out of the four, as a number between zero and sixty five thousand. The answer is an
 * `int` and the short is read unsigned, so a lane with its top bit set does not come out
 * negative, which is what the instruction's zero extension does. */
static __inline__ int __attribute__((__always_inline__))
_mm_extract_pi16(__m64 __a, const int __n)
{
  return (int)((__v4hu)__a)[__n & 3];
}

/* One short put back, the other three kept. */
static __inline__ __m64 __attribute__((__always_inline__))
_mm_insert_pi16(__m64 __a, const int __d, const int __n)
{
  __v4hi __answer = (__v4hi)__a;
  __answer[__n & 3] = (short)__d;
  return (__m64)__answer;
}

/* The bytes of the first operand whose matching byte in the second has its sign bit set, written
 * to the address given. The bytes whose sign bit is clear are not written at all, which is the
 * point of the operation and is why this is a loop over eight bytes rather than one store. */
static __inline__ void __attribute__((__always_inline__))
_mm_maskmove_si64(__m64 __a, __m64 __mask, char *__p)
{
  __v8qu __from = (__v8qu)__a, __which = (__v8qu)__mask;
  int __i;
  for (__i = 0; __i < 8; __i++)
    if ((__which[__i] & 0x80u) != 0u)
      __p[__i] = (char)__from[__i];
}

/* # The hints
 *
 * None of these four changes what a program computes. Each asks the machine to do something a
 * particular way, and doing it the ordinary way instead is always correct. */

/* A request that a cache line be fetched, which is exactly `__builtin_prefetch` with the selector
 * taken apart: the third bit says the line will be written and the low two say how long to keep
 * it. A macro so that a selector written as one of the `_MM_HINT_` names stays a constant, which
 * is what the builtin wants. */
#define _mm_prefetch(P, I) __builtin_prefetch((P), (((I) & 0xC) >> 2), ((I) & 0x3))

/* A hint to a processor spinning in a loop that it is spinning. Doing nothing is a correct
 * implementation of a hint, so this does nothing, and a program built with it spins slightly
 * hotter than one built by gcc and computes the same answers. */
static __inline__ void __attribute__((__always_inline__)) _mm_pause(void)
{
}

/* A fence over stores. `sfence` orders stores against stores and nothing else, and the fence
 * below orders everything against everything, which is stronger. Stronger is safe here: a program
 * that asked for its stores to be ordered gets that and more. It is also slower, and swapping it
 * for the narrower fence is worth doing once inline assembly can carry an instruction, which is
 * `tamnd/rucc#349`. */
static __inline__ void __attribute__((__always_inline__)) _mm_sfence(void)
{
  __atomic_thread_fence(__ATOMIC_SEQ_CST);
}

/* The stores that ask not to disturb the cache. An ordinary store is a correct implementation of
 * one, by the same argument as the hints above: the request is about what happens to the cache
 * and not about what ends up in memory. */
static __inline__ void __attribute__((__always_inline__)) _mm_stream_ps(float *__p, __m128 __a)
{
  *(__m128 *)__p = __a;
}

static __inline__ void __attribute__((__always_inline__)) _mm_stream_pi(__m64 *__p, __m64 __a)
{
  *__p = __a;
}

/* # The older names
 *
 * Eight of the conversions have a second spelling from before the names were regularized. They
 * are macros because a name standing for another name is what a macro is for. */
#define _mm_cvt_si2ss _mm_cvtsi32_ss
#define _mm_cvt_ss2si _mm_cvtss_si32
#define _mm_cvtt_ss2si _mm_cvttss_si32
#define _mm_cvt_pi2ps _mm_cvtpi32_ps
#define _mm_cvt_ps2pi _mm_cvtps_pi32
#define _mm_cvtt_ps2pi _mm_cvttps_pi32

#endif
