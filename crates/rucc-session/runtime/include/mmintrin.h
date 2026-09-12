/* mmintrin.h, the MMX intrinsics.
 *
 * MMX is the oldest of the vector extensions and the one every later header is built on top
 * of, which is why this file comes first even though almost nothing written today calls it
 * directly. `<xmmintrin.h>` includes it because several SSE intrinsics take or produce an
 * `__m64`, and `<emmintrin.h>` includes that in turn, so a program asking for SSE2 gets all
 * three.
 *
 * # These are C, not instructions
 *
 * Every intrinsic here is written as ordinary C over vector typed values. That is a real
 * choice and it has two consequences worth stating plainly.
 *
 * What a program computes is right. A vector type in this compiler is a value with lanes and
 * the operators over it mean what they mean in GCC, so `_mm_add_pi16` adds four shorts and
 * gives the four sums, and a test that checks the answer passes. Every intrinsic in this
 * file was checked against GCC 16.2.0 on the same inputs.
 *
 * What a program compiles to is not the instruction the name is short for. This compiler
 * takes vectors apart into their lanes before the back end sees them, so `_mm_add_pi16`
 * becomes four scalar additions rather than one `paddw`, and a program calling these in a
 * loop is slower than the same program built by GCC. That is `tamnd/rucc#200`, and until it
 * lands the honest description of this header is that it is correct and not fast.
 *
 * Nothing here uses an MMX register, so there is no register file to hand back to the x87
 * unit and `_mm_empty` has nothing to do. It is still provided, and still does nothing, so
 * that a program written the way MMX asks keeps working when the registers arrive.
 *
 * # Why `static __inline__` and not what GCC writes
 *
 * GCC spells these `extern __inline` with `__gnu_inline__`, which means the definition is
 * for inlining and no out of line copy is emitted, and relies on `__always_inline__` to make
 * sure every call is inlined. This compiler honours the first half and not yet the second,
 * so the same spelling here leaves every call that was not inlined pointing at a definition
 * nothing emitted. `static __inline__` says the same thing about visibility without that
 * gap: an inlined call is free and a call that was not inlined reaches a local copy. The
 * difference is `tamnd/rucc#1149` and this line goes back to GCC's spelling when it closes. */

#ifndef __RUCC_MMINTRIN_H
#define __RUCC_MMINTRIN_H

/* The lane types, named the way GCC names them so that a program reading a disassembly or a
 * header side by side sees the same words. The letter is the lane and the digit is how many:
 * `qi` is a byte, `hi` is a short, `si` is an int, `di` is a long long, and a `u` before it
 * makes the lane unsigned. */
typedef signed char __v8qi __attribute__((__vector_size__(8)));
typedef unsigned char __v8qu __attribute__((__vector_size__(8)));
typedef short __v4hi __attribute__((__vector_size__(8)));
typedef unsigned short __v4hu __attribute__((__vector_size__(8)));
typedef int __v2si __attribute__((__vector_size__(8)));
typedef unsigned int __v2su __attribute__((__vector_size__(8)));
typedef long long __v1di __attribute__((__vector_size__(8)));
typedef unsigned long long __v1du __attribute__((__vector_size__(8)));

/* The type an MMX intrinsic takes and gives back.
 *
 * `__may_alias__` is not decoration. A program is allowed to point an `__m64 *` at a buffer
 * of shorts and read it, which is the whole reason the type exists, and without the
 * attribute that read is one the aliasing rules say cannot happen and an optimizer is free
 * to move something across. */
typedef long long __m64 __attribute__((__vector_size__(8), __may_alias__));

/* The three saturations the packed arithmetic needs, each one clamping to the ends of a lane
 * rather than wrapping around it. The argument is `int` for all of them because every input
 * they are asked about fits in one: the widest is a pair of shorts added together. */
static __inline__ signed char __attribute__((__always_inline__))
__rucc_sat_qi(int __value)
{
  return __value < -128 ? (signed char)-128
                        : (__value > 127 ? (signed char)127 : (signed char)__value);
}

static __inline__ unsigned char __attribute__((__always_inline__))
__rucc_sat_qu(int __value)
{
  return __value < 0 ? (unsigned char)0
                     : (__value > 255 ? (unsigned char)255 : (unsigned char)__value);
}

static __inline__ short __attribute__((__always_inline__)) __rucc_sat_hi(int __value)
{
  return __value < -32768 ? (short)-32768
                          : (__value > 32767 ? (short)32767 : (short)__value);
}

static __inline__ unsigned short __attribute__((__always_inline__))
__rucc_sat_hu(int __value)
{
  return __value < 0 ? (unsigned short)0
                     : (__value > 65535 ? (unsigned short)65535 : (unsigned short)__value);
}

/* Hands back the x87 register file, which on this compiler was never taken.
 *
 * On hardware an MMX register is one of the x87 stack registers under another name, so a
 * function that used one has to say it is finished before anything reads a `double` again.
 * Nothing here puts a value in one, so there is nothing to say, and the right implementation
 * of `emms` is to do nothing at all rather than to refuse. */
static __inline__ void __attribute__((__always_inline__)) _mm_empty(void)
{
}

/* ---- Moving between an `__m64` and an ordinary integer ---- */

/* An `int` in the low half, zeroes in the high half. */
static __inline__ __m64 __attribute__((__always_inline__)) _mm_cvtsi32_si64(int __value)
{
  return (__m64)(__v2si){ __value, 0 };
}

/* The low half as an `int`, with the high half dropped. */
static __inline__ int __attribute__((__always_inline__)) _mm_cvtsi64_si32(__m64 __a)
{
  return ((__v2si)__a)[0];
}

#ifdef __x86_64__
/* All sixty four bits, read as one number and then as one vector. Both directions are a
 * reinterpretation and neither changes a bit. */
static __inline__ __m64 __attribute__((__always_inline__)) _mm_cvtsi64_m64(long long __value)
{
  return (__m64)(__v1di){ __value };
}

static __inline__ long long __attribute__((__always_inline__)) _mm_cvtm64_si64(__m64 __a)
{
  return ((__v1di)__a)[0];
}
#endif

/* ---- Narrowing two vectors into one ---- */

/* Eight shorts as eight bytes, each one clamped to a signed byte. */
static __inline__ __m64 __attribute__((__always_inline__))
_mm_packs_pi16(__m64 __a, __m64 __b)
{
  __v4hi __x = (__v4hi)__a, __y = (__v4hi)__b;
  return (__m64)(__v8qi){ __rucc_sat_qi(__x[0]), __rucc_sat_qi(__x[1]),
                          __rucc_sat_qi(__x[2]), __rucc_sat_qi(__x[3]),
                          __rucc_sat_qi(__y[0]), __rucc_sat_qi(__y[1]),
                          __rucc_sat_qi(__y[2]), __rucc_sat_qi(__y[3]) };
}

/* Four ints as four shorts, each one clamped to a signed short. */
static __inline__ __m64 __attribute__((__always_inline__))
_mm_packs_pi32(__m64 __a, __m64 __b)
{
  __v2si __x = (__v2si)__a, __y = (__v2si)__b;
  return (__m64)(__v4hi){ __rucc_sat_hi(__x[0]), __rucc_sat_hi(__x[1]),
                          __rucc_sat_hi(__y[0]), __rucc_sat_hi(__y[1]) };
}

/* Eight shorts as eight bytes, each one clamped to an unsigned byte. The inputs are still
 * signed, so a negative short becomes zero rather than a large byte. */
static __inline__ __m64 __attribute__((__always_inline__))
_mm_packs_pu16(__m64 __a, __m64 __b)
{
  __v4hi __x = (__v4hi)__a, __y = (__v4hi)__b;
  return (__m64)(__v8qu){ __rucc_sat_qu(__x[0]), __rucc_sat_qu(__x[1]),
                          __rucc_sat_qu(__x[2]), __rucc_sat_qu(__x[3]),
                          __rucc_sat_qu(__y[0]), __rucc_sat_qu(__y[1]),
                          __rucc_sat_qu(__y[2]), __rucc_sat_qu(__y[3]) };
}

/* ---- Interleaving two vectors ---- */

/* The low half of each, one lane from `__a` then one from `__b`, all the way across. */
static __inline__ __m64 __attribute__((__always_inline__))
_mm_unpacklo_pi8(__m64 __a, __m64 __b)
{
  __v8qi __x = (__v8qi)__a, __y = (__v8qi)__b;
  return (__m64)(__v8qi){ __x[0], __y[0], __x[1], __y[1],
                          __x[2], __y[2], __x[3], __y[3] };
}

static __inline__ __m64 __attribute__((__always_inline__))
_mm_unpackhi_pi8(__m64 __a, __m64 __b)
{
  __v8qi __x = (__v8qi)__a, __y = (__v8qi)__b;
  return (__m64)(__v8qi){ __x[4], __y[4], __x[5], __y[5],
                          __x[6], __y[6], __x[7], __y[7] };
}

static __inline__ __m64 __attribute__((__always_inline__))
_mm_unpacklo_pi16(__m64 __a, __m64 __b)
{
  __v4hi __x = (__v4hi)__a, __y = (__v4hi)__b;
  return (__m64)(__v4hi){ __x[0], __y[0], __x[1], __y[1] };
}

static __inline__ __m64 __attribute__((__always_inline__))
_mm_unpackhi_pi16(__m64 __a, __m64 __b)
{
  __v4hi __x = (__v4hi)__a, __y = (__v4hi)__b;
  return (__m64)(__v4hi){ __x[2], __y[2], __x[3], __y[3] };
}

static __inline__ __m64 __attribute__((__always_inline__))
_mm_unpacklo_pi32(__m64 __a, __m64 __b)
{
  return (__m64)(__v2si){ ((__v2si)__a)[0], ((__v2si)__b)[0] };
}

static __inline__ __m64 __attribute__((__always_inline__))
_mm_unpackhi_pi32(__m64 __a, __m64 __b)
{
  return (__m64)(__v2si){ ((__v2si)__a)[1], ((__v2si)__b)[1] };
}

/* ---- Addition and subtraction ---- */

static __inline__ __m64 __attribute__((__always_inline__)) _mm_add_pi8(__m64 __a, __m64 __b)
{
  return (__m64)((__v8qu)__a + (__v8qu)__b);
}

static __inline__ __m64 __attribute__((__always_inline__)) _mm_add_pi16(__m64 __a, __m64 __b)
{
  return (__m64)((__v4hu)__a + (__v4hu)__b);
}

static __inline__ __m64 __attribute__((__always_inline__)) _mm_add_pi32(__m64 __a, __m64 __b)
{
  return (__m64)((__v2su)__a + (__v2su)__b);
}

static __inline__ __m64 __attribute__((__always_inline__)) _mm_add_si64(__m64 __a, __m64 __b)
{
  return (__m64)((__v1du)__a + (__v1du)__b);
}

static __inline__ __m64 __attribute__((__always_inline__)) _mm_sub_pi8(__m64 __a, __m64 __b)
{
  return (__m64)((__v8qu)__a - (__v8qu)__b);
}

static __inline__ __m64 __attribute__((__always_inline__)) _mm_sub_pi16(__m64 __a, __m64 __b)
{
  return (__m64)((__v4hu)__a - (__v4hu)__b);
}

static __inline__ __m64 __attribute__((__always_inline__)) _mm_sub_pi32(__m64 __a, __m64 __b)
{
  return (__m64)((__v2su)__a - (__v2su)__b);
}

static __inline__ __m64 __attribute__((__always_inline__)) _mm_sub_si64(__m64 __a, __m64 __b)
{
  return (__m64)((__v1du)__a - (__v1du)__b);
}

/* The saturating forms, which stop at the end of the lane instead of wrapping past it. The
 * arithmetic is done a lane at a time in `int`, where nothing overflows, and the clamp is
 * applied to the wide answer. */
static __inline__ __m64 __attribute__((__always_inline__)) _mm_adds_pi8(__m64 __a, __m64 __b)
{
  __v8qi __x = (__v8qi)__a, __y = (__v8qi)__b;
  return (__m64)(__v8qi){
    __rucc_sat_qi(__x[0] + __y[0]), __rucc_sat_qi(__x[1] + __y[1]),
    __rucc_sat_qi(__x[2] + __y[2]), __rucc_sat_qi(__x[3] + __y[3]),
    __rucc_sat_qi(__x[4] + __y[4]), __rucc_sat_qi(__x[5] + __y[5]),
    __rucc_sat_qi(__x[6] + __y[6]), __rucc_sat_qi(__x[7] + __y[7])
  };
}

static __inline__ __m64 __attribute__((__always_inline__)) _mm_adds_pi16(__m64 __a, __m64 __b)
{
  __v4hi __x = (__v4hi)__a, __y = (__v4hi)__b;
  return (__m64)(__v4hi){
    __rucc_sat_hi(__x[0] + __y[0]), __rucc_sat_hi(__x[1] + __y[1]),
    __rucc_sat_hi(__x[2] + __y[2]), __rucc_sat_hi(__x[3] + __y[3])
  };
}

static __inline__ __m64 __attribute__((__always_inline__)) _mm_adds_pu8(__m64 __a, __m64 __b)
{
  __v8qu __x = (__v8qu)__a, __y = (__v8qu)__b;
  return (__m64)(__v8qu){
    __rucc_sat_qu(__x[0] + __y[0]), __rucc_sat_qu(__x[1] + __y[1]),
    __rucc_sat_qu(__x[2] + __y[2]), __rucc_sat_qu(__x[3] + __y[3]),
    __rucc_sat_qu(__x[4] + __y[4]), __rucc_sat_qu(__x[5] + __y[5]),
    __rucc_sat_qu(__x[6] + __y[6]), __rucc_sat_qu(__x[7] + __y[7])
  };
}

static __inline__ __m64 __attribute__((__always_inline__)) _mm_adds_pu16(__m64 __a, __m64 __b)
{
  __v4hu __x = (__v4hu)__a, __y = (__v4hu)__b;
  return (__m64)(__v4hu){
    __rucc_sat_hu((int)__x[0] + (int)__y[0]), __rucc_sat_hu((int)__x[1] + (int)__y[1]),
    __rucc_sat_hu((int)__x[2] + (int)__y[2]), __rucc_sat_hu((int)__x[3] + (int)__y[3])
  };
}

static __inline__ __m64 __attribute__((__always_inline__)) _mm_subs_pi8(__m64 __a, __m64 __b)
{
  __v8qi __x = (__v8qi)__a, __y = (__v8qi)__b;
  return (__m64)(__v8qi){
    __rucc_sat_qi(__x[0] - __y[0]), __rucc_sat_qi(__x[1] - __y[1]),
    __rucc_sat_qi(__x[2] - __y[2]), __rucc_sat_qi(__x[3] - __y[3]),
    __rucc_sat_qi(__x[4] - __y[4]), __rucc_sat_qi(__x[5] - __y[5]),
    __rucc_sat_qi(__x[6] - __y[6]), __rucc_sat_qi(__x[7] - __y[7])
  };
}

static __inline__ __m64 __attribute__((__always_inline__)) _mm_subs_pi16(__m64 __a, __m64 __b)
{
  __v4hi __x = (__v4hi)__a, __y = (__v4hi)__b;
  return (__m64)(__v4hi){
    __rucc_sat_hi(__x[0] - __y[0]), __rucc_sat_hi(__x[1] - __y[1]),
    __rucc_sat_hi(__x[2] - __y[2]), __rucc_sat_hi(__x[3] - __y[3])
  };
}

static __inline__ __m64 __attribute__((__always_inline__)) _mm_subs_pu8(__m64 __a, __m64 __b)
{
  __v8qu __x = (__v8qu)__a, __y = (__v8qu)__b;
  return (__m64)(__v8qu){
    __rucc_sat_qu(__x[0] - __y[0]), __rucc_sat_qu(__x[1] - __y[1]),
    __rucc_sat_qu(__x[2] - __y[2]), __rucc_sat_qu(__x[3] - __y[3]),
    __rucc_sat_qu(__x[4] - __y[4]), __rucc_sat_qu(__x[5] - __y[5]),
    __rucc_sat_qu(__x[6] - __y[6]), __rucc_sat_qu(__x[7] - __y[7])
  };
}

static __inline__ __m64 __attribute__((__always_inline__)) _mm_subs_pu16(__m64 __a, __m64 __b)
{
  __v4hu __x = (__v4hu)__a, __y = (__v4hu)__b;
  return (__m64)(__v4hu){
    __rucc_sat_hu((int)__x[0] - (int)__y[0]), __rucc_sat_hu((int)__x[1] - (int)__y[1]),
    __rucc_sat_hu((int)__x[2] - (int)__y[2]), __rucc_sat_hu((int)__x[3] - (int)__y[3])
  };
}

/* ---- Multiplication ---- */

/* Four products of shorts, added in adjacent pairs, giving two ints. The sum is done in
 * unsigned arithmetic because the one input that overflows it, two lanes of the smallest
 * short squared, is a case the instruction wraps and C would call undefined. */
static __inline__ __m64 __attribute__((__always_inline__)) _mm_madd_pi16(__m64 __a, __m64 __b)
{
  __v4hi __x = (__v4hi)__a, __y = (__v4hi)__b;
  unsigned __low = (unsigned)(__x[0] * __y[0]) + (unsigned)(__x[1] * __y[1]);
  unsigned __high = (unsigned)(__x[2] * __y[2]) + (unsigned)(__x[3] * __y[3]);
  return (__m64)(__v2si){ (int)__low, (int)__high };
}

/* The top half of each signed product, which is the half an ordinary multiply throws away. */
static __inline__ __m64 __attribute__((__always_inline__))
_mm_mulhi_pi16(__m64 __a, __m64 __b)
{
  __v4hi __x = (__v4hi)__a, __y = (__v4hi)__b;
  return (__m64)(__v4hi){ (short)((__x[0] * __y[0]) >> 16), (short)((__x[1] * __y[1]) >> 16),
                          (short)((__x[2] * __y[2]) >> 16), (short)((__x[3] * __y[3]) >> 16) };
}

/* The bottom half of each product, which is the same bits whether the lanes are read as
 * signed or unsigned, so the unsigned type is used and nothing overflows. */
static __inline__ __m64 __attribute__((__always_inline__))
_mm_mullo_pi16(__m64 __a, __m64 __b)
{
  return (__m64)((__v4hu)__a * (__v4hu)__b);
}

/* ---- The bitwise operations, which have no lanes ---- */

static __inline__ __m64 __attribute__((__always_inline__)) _mm_and_si64(__m64 __a, __m64 __b)
{
  return (__m64)((__v1du)__a & (__v1du)__b);
}

/* `__b` without the bits `__a` has, which is the operand order the instruction uses and the
 * one people get backwards. */
static __inline__ __m64 __attribute__((__always_inline__))
_mm_andnot_si64(__m64 __a, __m64 __b)
{
  return (__m64)(~(__v1du)__a & (__v1du)__b);
}

static __inline__ __m64 __attribute__((__always_inline__)) _mm_or_si64(__m64 __a, __m64 __b)
{
  return (__m64)((__v1du)__a | (__v1du)__b);
}

static __inline__ __m64 __attribute__((__always_inline__)) _mm_xor_si64(__m64 __a, __m64 __b)
{
  return (__m64)((__v1du)__a ^ (__v1du)__b);
}

/* ---- Comparison, which answers in lanes of all ones or all zeroes ---- */

static __inline__ __m64 __attribute__((__always_inline__))
_mm_cmpeq_pi8(__m64 __a, __m64 __b)
{
  return (__m64)((__v8qi)__a == (__v8qi)__b);
}

static __inline__ __m64 __attribute__((__always_inline__))
_mm_cmpeq_pi16(__m64 __a, __m64 __b)
{
  return (__m64)((__v4hi)__a == (__v4hi)__b);
}

static __inline__ __m64 __attribute__((__always_inline__))
_mm_cmpeq_pi32(__m64 __a, __m64 __b)
{
  return (__m64)((__v2si)__a == (__v2si)__b);
}

/* There is no unsigned compare in MMX and there is no `cmplt` either, because swapping the
 * operands of `cmpgt` is the same answer and one instruction is enough. */
static __inline__ __m64 __attribute__((__always_inline__))
_mm_cmpgt_pi8(__m64 __a, __m64 __b)
{
  return (__m64)((__v8qi)__a > (__v8qi)__b);
}

static __inline__ __m64 __attribute__((__always_inline__))
_mm_cmpgt_pi16(__m64 __a, __m64 __b)
{
  return (__m64)((__v4hi)__a > (__v4hi)__b);
}

static __inline__ __m64 __attribute__((__always_inline__))
_mm_cmpgt_pi32(__m64 __a, __m64 __b)
{
  return (__m64)((__v2si)__a > (__v2si)__b);
}

/* ---- Shifts ---- */

/* A shift count wider than the lane is not undefined here the way it is in C. Every bit has
 * moved out of the lane, so a left or a logical right shift gives zero and an arithmetic
 * right shift gives the sign bit repeated, and the hardware says so rather than leaving it
 * open. The count is read as an unsigned sixty four bit number, which is why a negative
 * count reaches the same answer as an enormous one.
 *
 * Each operation is written once and called twice, because the form that takes the count in
 * a vector and the form that takes it as an immediate differ only in where they read it. */
static __inline__ __m64 __attribute__((__always_inline__))
__rucc_sllw(__m64 __a, unsigned long long __n)
{
  __v4hu __x = (__v4hu)__a;
  if (__n > 15)
    return (__m64)(__v4hu){ 0, 0, 0, 0 };
  return (__m64)(__v4hu){ (unsigned short)(__x[0] << __n), (unsigned short)(__x[1] << __n),
                          (unsigned short)(__x[2] << __n), (unsigned short)(__x[3] << __n) };
}

static __inline__ __m64 __attribute__((__always_inline__))
__rucc_srlw(__m64 __a, unsigned long long __n)
{
  __v4hu __x = (__v4hu)__a;
  if (__n > 15)
    return (__m64)(__v4hu){ 0, 0, 0, 0 };
  return (__m64)(__v4hu){ (unsigned short)(__x[0] >> __n), (unsigned short)(__x[1] >> __n),
                          (unsigned short)(__x[2] >> __n), (unsigned short)(__x[3] >> __n) };
}

static __inline__ __m64 __attribute__((__always_inline__))
__rucc_sraw(__m64 __a, unsigned long long __n)
{
  __v4hi __x = (__v4hi)__a;
  int __by = __n > 15 ? 15 : (int)__n;
  return (__m64)(__v4hi){ (short)(__x[0] >> __by), (short)(__x[1] >> __by),
                          (short)(__x[2] >> __by), (short)(__x[3] >> __by) };
}

static __inline__ __m64 __attribute__((__always_inline__))
__rucc_slld(__m64 __a, unsigned long long __n)
{
  __v2su __x = (__v2su)__a;
  if (__n > 31)
    return (__m64)(__v2su){ 0, 0 };
  return (__m64)(__v2su){ __x[0] << __n, __x[1] << __n };
}

static __inline__ __m64 __attribute__((__always_inline__))
__rucc_srld(__m64 __a, unsigned long long __n)
{
  __v2su __x = (__v2su)__a;
  if (__n > 31)
    return (__m64)(__v2su){ 0, 0 };
  return (__m64)(__v2su){ __x[0] >> __n, __x[1] >> __n };
}

static __inline__ __m64 __attribute__((__always_inline__))
__rucc_srad(__m64 __a, unsigned long long __n)
{
  __v2si __x = (__v2si)__a;
  int __by = __n > 31 ? 31 : (int)__n;
  return (__m64)(__v2si){ __x[0] >> __by, __x[1] >> __by };
}

static __inline__ __m64 __attribute__((__always_inline__))
__rucc_sllq(__m64 __a, unsigned long long __n)
{
  if (__n > 63)
    return (__m64)(__v1du){ 0 };
  return (__m64)(__v1du){ ((__v1du)__a)[0] << __n };
}

static __inline__ __m64 __attribute__((__always_inline__))
__rucc_srlq(__m64 __a, unsigned long long __n)
{
  if (__n > 63)
    return (__m64)(__v1du){ 0 };
  return (__m64)(__v1du){ ((__v1du)__a)[0] >> __n };
}

/* The count as the whole of a vector read as one unsigned number, which is where the forms
 * without an `i` in the name take it from. */
static __inline__ unsigned long long __attribute__((__always_inline__))
__rucc_count(__m64 __count)
{
  return ((__v1du)__count)[0];
}

static __inline__ __m64 __attribute__((__always_inline__))
_mm_sll_pi16(__m64 __a, __m64 __count)
{
  return __rucc_sllw(__a, __rucc_count(__count));
}

static __inline__ __m64 __attribute__((__always_inline__)) _mm_slli_pi16(__m64 __a, int __n)
{
  return __rucc_sllw(__a, (unsigned long long)(unsigned int)__n);
}

static __inline__ __m64 __attribute__((__always_inline__))
_mm_srl_pi16(__m64 __a, __m64 __count)
{
  return __rucc_srlw(__a, __rucc_count(__count));
}

static __inline__ __m64 __attribute__((__always_inline__)) _mm_srli_pi16(__m64 __a, int __n)
{
  return __rucc_srlw(__a, (unsigned long long)(unsigned int)__n);
}

static __inline__ __m64 __attribute__((__always_inline__))
_mm_sra_pi16(__m64 __a, __m64 __count)
{
  return __rucc_sraw(__a, __rucc_count(__count));
}

static __inline__ __m64 __attribute__((__always_inline__)) _mm_srai_pi16(__m64 __a, int __n)
{
  return __rucc_sraw(__a, (unsigned long long)(unsigned int)__n);
}

static __inline__ __m64 __attribute__((__always_inline__))
_mm_sll_pi32(__m64 __a, __m64 __count)
{
  return __rucc_slld(__a, __rucc_count(__count));
}

static __inline__ __m64 __attribute__((__always_inline__)) _mm_slli_pi32(__m64 __a, int __n)
{
  return __rucc_slld(__a, (unsigned long long)(unsigned int)__n);
}

static __inline__ __m64 __attribute__((__always_inline__))
_mm_srl_pi32(__m64 __a, __m64 __count)
{
  return __rucc_srld(__a, __rucc_count(__count));
}

static __inline__ __m64 __attribute__((__always_inline__)) _mm_srli_pi32(__m64 __a, int __n)
{
  return __rucc_srld(__a, (unsigned long long)(unsigned int)__n);
}

static __inline__ __m64 __attribute__((__always_inline__))
_mm_sra_pi32(__m64 __a, __m64 __count)
{
  return __rucc_srad(__a, __rucc_count(__count));
}

static __inline__ __m64 __attribute__((__always_inline__)) _mm_srai_pi32(__m64 __a, int __n)
{
  return __rucc_srad(__a, (unsigned long long)(unsigned int)__n);
}

static __inline__ __m64 __attribute__((__always_inline__))
_mm_sll_si64(__m64 __a, __m64 __count)
{
  return __rucc_sllq(__a, __rucc_count(__count));
}

static __inline__ __m64 __attribute__((__always_inline__)) _mm_slli_si64(__m64 __a, int __n)
{
  return __rucc_sllq(__a, (unsigned long long)(unsigned int)__n);
}

static __inline__ __m64 __attribute__((__always_inline__))
_mm_srl_si64(__m64 __a, __m64 __count)
{
  return __rucc_srlq(__a, __rucc_count(__count));
}

static __inline__ __m64 __attribute__((__always_inline__)) _mm_srli_si64(__m64 __a, int __n)
{
  return __rucc_srlq(__a, (unsigned long long)(unsigned int)__n);
}

/* ---- Making a vector out of nothing, or out of scalars ---- */

static __inline__ __m64 __attribute__((__always_inline__)) _mm_setzero_si64(void)
{
  return (__m64)(__v1di){ 0 };
}

/* The arguments run from the highest lane to the lowest, which is the order the lanes appear
 * in when the value is written down as one number and is the reverse of the order they are
 * in in memory. `_mm_setr_*` is the same call with the arguments the other way round, for
 * programs that would rather write them in memory order. */
static __inline__ __m64 __attribute__((__always_inline__))
_mm_set_pi8(char __b7, char __b6, char __b5, char __b4, char __b3, char __b2, char __b1,
            char __b0)
{
  return (__m64)(__v8qi){ __b0, __b1, __b2, __b3, __b4, __b5, __b6, __b7 };
}

static __inline__ __m64 __attribute__((__always_inline__))
_mm_set_pi16(short __w3, short __w2, short __w1, short __w0)
{
  return (__m64)(__v4hi){ __w0, __w1, __w2, __w3 };
}

static __inline__ __m64 __attribute__((__always_inline__))
_mm_set_pi32(int __d1, int __d0)
{
  return (__m64)(__v2si){ __d0, __d1 };
}

static __inline__ __m64 __attribute__((__always_inline__)) _mm_set_pi64x(long long __q)
{
  return (__m64)(__v1di){ __q };
}

static __inline__ __m64 __attribute__((__always_inline__))
_mm_setr_pi8(char __b0, char __b1, char __b2, char __b3, char __b4, char __b5, char __b6,
             char __b7)
{
  return (__m64)(__v8qi){ __b0, __b1, __b2, __b3, __b4, __b5, __b6, __b7 };
}

static __inline__ __m64 __attribute__((__always_inline__))
_mm_setr_pi16(short __w0, short __w1, short __w2, short __w3)
{
  return (__m64)(__v4hi){ __w0, __w1, __w2, __w3 };
}

static __inline__ __m64 __attribute__((__always_inline__))
_mm_setr_pi32(int __d0, int __d1)
{
  return (__m64)(__v2si){ __d0, __d1 };
}

static __inline__ __m64 __attribute__((__always_inline__)) _mm_set1_pi8(char __b)
{
  return (__m64)(__v8qi){ __b, __b, __b, __b, __b, __b, __b, __b };
}

static __inline__ __m64 __attribute__((__always_inline__)) _mm_set1_pi16(short __w)
{
  return (__m64)(__v4hi){ __w, __w, __w, __w };
}

static __inline__ __m64 __attribute__((__always_inline__)) _mm_set1_pi32(int __d)
{
  return (__m64)(__v2si){ __d, __d };
}

/* ---- The older spellings ---- */

/* Every MMX intrinsic has a second name, from before the `_mm_` convention settled, and the
 * second name is the instruction with a `_m_p` in front of it. They are macros rather than
 * functions because a name standing for another name is what a macro is for, and an object
 * like macro leaves a call written any way round it alone. */
#define _m_empty _mm_empty
#define _m_from_int _mm_cvtsi32_si64
#define _m_to_int _mm_cvtsi64_si32
#define _m_packsswb _mm_packs_pi16
#define _m_packssdw _mm_packs_pi32
#define _m_packuswb _mm_packs_pu16
#define _m_punpckhbw _mm_unpackhi_pi8
#define _m_punpckhwd _mm_unpackhi_pi16
#define _m_punpckhdq _mm_unpackhi_pi32
#define _m_punpcklbw _mm_unpacklo_pi8
#define _m_punpcklwd _mm_unpacklo_pi16
#define _m_punpckldq _mm_unpacklo_pi32
#define _m_paddb _mm_add_pi8
#define _m_paddw _mm_add_pi16
#define _m_paddd _mm_add_pi32
#define _m_paddsb _mm_adds_pi8
#define _m_paddsw _mm_adds_pi16
#define _m_paddusb _mm_adds_pu8
#define _m_paddusw _mm_adds_pu16
#define _m_psubb _mm_sub_pi8
#define _m_psubw _mm_sub_pi16
#define _m_psubd _mm_sub_pi32
#define _m_psubsb _mm_subs_pi8
#define _m_psubsw _mm_subs_pi16
#define _m_psubusb _mm_subs_pu8
#define _m_psubusw _mm_subs_pu16
#define _m_pmaddwd _mm_madd_pi16
#define _m_pmulhw _mm_mulhi_pi16
#define _m_pmullw _mm_mullo_pi16
#define _m_pand _mm_and_si64
#define _m_pandn _mm_andnot_si64
#define _m_por _mm_or_si64
#define _m_pxor _mm_xor_si64
#define _m_pcmpeqb _mm_cmpeq_pi8
#define _m_pcmpeqw _mm_cmpeq_pi16
#define _m_pcmpeqd _mm_cmpeq_pi32
#define _m_pcmpgtb _mm_cmpgt_pi8
#define _m_pcmpgtw _mm_cmpgt_pi16
#define _m_pcmpgtd _mm_cmpgt_pi32
#define _m_psllw _mm_sll_pi16
#define _m_psllwi _mm_slli_pi16
#define _m_pslld _mm_sll_pi32
#define _m_pslldi _mm_slli_pi32
#define _m_psllq _mm_sll_si64
#define _m_psllqi _mm_slli_si64
#define _m_psrlw _mm_srl_pi16
#define _m_psrlwi _mm_srli_pi16
#define _m_psrld _mm_srl_pi32
#define _m_psrldi _mm_srli_pi32
#define _m_psrlq _mm_srl_si64
#define _m_psrlqi _mm_srli_si64
#define _m_psraw _mm_sra_pi16
#define _m_psrawi _mm_srai_pi16
#define _m_psrad _mm_sra_pi32
#define _m_psradi _mm_srai_pi32

#ifdef __x86_64__
#define _m_from_int64 _mm_cvtsi64_m64
#define _m_to_int64 _mm_cvtm64_si64
#define _mm_cvtsi64x_si64 _mm_cvtsi64_m64
#define _mm_cvtsi64_si64x _mm_cvtm64_si64
#endif

#endif
