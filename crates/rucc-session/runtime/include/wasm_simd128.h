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

#ifndef __RUCC_WASM_SIMD128_H
#define __RUCC_WASM_SIMD128_H

#if !defined(__wasm__)
#error "wasm_simd128.h is for WebAssembly"
#endif

#include <stdbool.h>
#include <stdint.h>

typedef int32_t v128_t __attribute__((__vector_size__(16), __aligned__(16)));
typedef int32_t __v128_u __attribute__((__vector_size__(16), __aligned__(1)));
typedef signed char __i8x16 __attribute__((__vector_size__(16), __aligned__(16)));
typedef short __i16x8 __attribute__((__vector_size__(16), __aligned__(16)));
typedef int __i32x4 __attribute__((__vector_size__(16), __aligned__(16)));
typedef long long __i64x2 __attribute__((__vector_size__(16), __aligned__(16)));
typedef unsigned char __u8x16 __attribute__((__vector_size__(16), __aligned__(16)));
typedef unsigned short __u16x8 __attribute__((__vector_size__(16), __aligned__(16)));
typedef unsigned int __u32x4 __attribute__((__vector_size__(16), __aligned__(16)));
typedef unsigned long long __u64x2 __attribute__((__vector_size__(16), __aligned__(16)));
typedef float __f32x4 __attribute__((__vector_size__(16), __aligned__(16)));
typedef double __f64x2 __attribute__((__vector_size__(16), __aligned__(16)));
typedef signed char __i8x8 __attribute__((__vector_size__(8), __aligned__(8)));
typedef unsigned char __u8x8 __attribute__((__vector_size__(8), __aligned__(8)));
typedef short __i16x4 __attribute__((__vector_size__(8), __aligned__(8)));
typedef unsigned short __u16x4 __attribute__((__vector_size__(8), __aligned__(8)));
typedef int __i32x2 __attribute__((__vector_size__(8), __aligned__(8)));
typedef unsigned int __u32x2 __attribute__((__vector_size__(8), __aligned__(8)));
typedef float __f32x2 __attribute__((__vector_size__(8), __aligned__(8)));

/* A float rounded to an integer in C. Wasm rounds each operation to the nearest even, so adding
 * 2^23 to a float below it and taking it away again leaves the nearest integer, ties to even.
 * The other roundings start from that one. The sign of the answer is the sign of the operand,
 * which keeps -0.0 and the negative numbers that round to zero. A NaN comes back quiet. */
static __inline__ float __rucc_wasm_nearest_f32(float __x) {
  float __a = __builtin_fabsf(__x);
  if (__a < 0x1p23f) return __builtin_copysignf((__a + 0x1p23f) - 0x1p23f, __x);
  return __x != __x ? __x + __x : __x;
}
static __inline__ float __rucc_wasm_floor_f32(float __x) {
  float __r = __rucc_wasm_nearest_f32(__x);
  return __builtin_copysignf(__r > __x ? __r - 1.0f : __r, __x);
}
static __inline__ float __rucc_wasm_ceil_f32(float __x) {
  float __r = __rucc_wasm_nearest_f32(__x);
  return __builtin_copysignf(__r < __x ? __r + 1.0f : __r, __x);
}
static __inline__ float __rucc_wasm_trunc_f32(float __x) {
  float __a = __builtin_fabsf(__x);
  float __r = __rucc_wasm_nearest_f32(__a);
  return __builtin_copysignf(__r > __a ? __r - 1.0f : __r, __x);
}
static __inline__ double __rucc_wasm_nearest_f64(double __x) {
  double __a = __builtin_fabs(__x);
  if (__a < 0x1p52) return __builtin_copysign((__a + 0x1p52) - 0x1p52, __x);
  return __x != __x ? __x + __x : __x;
}
static __inline__ double __rucc_wasm_floor_f64(double __x) {
  double __r = __rucc_wasm_nearest_f64(__x);
  return __builtin_copysign(__r > __x ? __r - 1.0 : __r, __x);
}
static __inline__ double __rucc_wasm_ceil_f64(double __x) {
  double __r = __rucc_wasm_nearest_f64(__x);
  return __builtin_copysign(__r < __x ? __r + 1.0 : __r, __x);
}
static __inline__ double __rucc_wasm_trunc_f64(double __x) {
  double __a = __builtin_fabs(__x);
  double __r = __rucc_wasm_nearest_f64(__a);
  return __builtin_copysign(__r > __a ? __r - 1.0 : __r, __x);
}

/* Loads and stores. Through memcpy, since none of them has to be aligned. */
static __inline__ v128_t wasm_v128_load(const void *__mem) {
  v128_t __r;
  __builtin_memcpy(&__r, __mem, 16);
  return __r;
}
static __inline__ v128_t wasm_v128_load8_splat(const void *__mem) {
  uint8_t __v;
  __builtin_memcpy(&__v, __mem, sizeof __v);
  return (v128_t)(__u8x16){__v, __v, __v, __v, __v, __v, __v, __v, __v, __v, __v, __v, __v, __v,
      __v, __v};
}
static __inline__ v128_t wasm_v128_load16_splat(const void *__mem) {
  uint16_t __v;
  __builtin_memcpy(&__v, __mem, sizeof __v);
  return (v128_t)(__u16x8){__v, __v, __v, __v, __v, __v, __v, __v};
}
static __inline__ v128_t wasm_v128_load32_splat(const void *__mem) {
  uint32_t __v;
  __builtin_memcpy(&__v, __mem, sizeof __v);
  return (v128_t)(__u32x4){__v, __v, __v, __v};
}
static __inline__ v128_t wasm_v128_load64_splat(const void *__mem) {
  uint64_t __v;
  __builtin_memcpy(&__v, __mem, sizeof __v);
  return (v128_t)(__u64x2){__v, __v};
}
static __inline__ v128_t wasm_i16x8_load8x8(const void *__mem) {
  int8_t __v[8];
  __builtin_memcpy(__v, __mem, sizeof __v);
  __i16x8 __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __v[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_u16x8_load8x8(const void *__mem) {
  uint8_t __v[8];
  __builtin_memcpy(__v, __mem, sizeof __v);
  __u16x8 __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __v[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_i32x4_load16x4(const void *__mem) {
  int16_t __v[4];
  __builtin_memcpy(__v, __mem, sizeof __v);
  __i32x4 __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __v[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_u32x4_load16x4(const void *__mem) {
  uint16_t __v[4];
  __builtin_memcpy(__v, __mem, sizeof __v);
  __u32x4 __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __v[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_i64x2_load32x2(const void *__mem) {
  int32_t __v[2];
  __builtin_memcpy(__v, __mem, sizeof __v);
  __i64x2 __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __v[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_u64x2_load32x2(const void *__mem) {
  uint32_t __v[2];
  __builtin_memcpy(__v, __mem, sizeof __v);
  __u64x2 __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __v[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_v128_load32_zero(const void *__mem) {
  uint32_t __v;
  __builtin_memcpy(&__v, __mem, sizeof __v);
  return (v128_t)(__u32x4){__v, 0, 0, 0};
}
static __inline__ v128_t wasm_v128_load64_zero(const void *__mem) {
  uint64_t __v;
  __builtin_memcpy(&__v, __mem, sizeof __v);
  return (v128_t)(__u64x2){__v, 0};
}
static __inline__ v128_t wasm_v128_load8_lane(const void *__mem, v128_t __vec, int __i) {
  uint8_t __v;
  __builtin_memcpy(&__v, __mem, sizeof __v);
  __u8x16 __r = (__u8x16)__vec;
  __r[__i] = __v;
  return (v128_t)__r;
}
static __inline__ v128_t wasm_v128_load16_lane(const void *__mem, v128_t __vec, int __i) {
  uint16_t __v;
  __builtin_memcpy(&__v, __mem, sizeof __v);
  __u16x8 __r = (__u16x8)__vec;
  __r[__i] = __v;
  return (v128_t)__r;
}
static __inline__ v128_t wasm_v128_load32_lane(const void *__mem, v128_t __vec, int __i) {
  uint32_t __v;
  __builtin_memcpy(&__v, __mem, sizeof __v);
  __u32x4 __r = (__u32x4)__vec;
  __r[__i] = __v;
  return (v128_t)__r;
}
static __inline__ v128_t wasm_v128_load64_lane(const void *__mem, v128_t __vec, int __i) {
  uint64_t __v;
  __builtin_memcpy(&__v, __mem, sizeof __v);
  __u64x2 __r = (__u64x2)__vec;
  __r[__i] = __v;
  return (v128_t)__r;
}
static __inline__ void wasm_v128_store(void *__mem, v128_t __a) {
  __builtin_memcpy(__mem, &__a, 16);
}
static __inline__ void wasm_v128_store8_lane(void *__mem, v128_t __vec, int __i) {
  __u8x16 __x = (__u8x16)__vec;
  uint8_t __v = __x[__i];
  __builtin_memcpy(__mem, &__v, sizeof __v);
}
static __inline__ void wasm_v128_store16_lane(void *__mem, v128_t __vec, int __i) {
  __u16x8 __x = (__u16x8)__vec;
  uint16_t __v = __x[__i];
  __builtin_memcpy(__mem, &__v, sizeof __v);
}
static __inline__ void wasm_v128_store32_lane(void *__mem, v128_t __vec, int __i) {
  __u32x4 __x = (__u32x4)__vec;
  uint32_t __v = __x[__i];
  __builtin_memcpy(__mem, &__v, sizeof __v);
}
static __inline__ void wasm_v128_store64_lane(void *__mem, v128_t __vec, int __i) {
  __u64x2 __x = (__u64x2)__vec;
  uint64_t __v = __x[__i];
  __builtin_memcpy(__mem, &__v, sizeof __v);
}

/* Making vectors and taking them apart. The const forms are the same as the others. */
static __inline__ v128_t wasm_i8x16_make(int8_t __c0, int8_t __c1, int8_t __c2, int8_t __c3,
    int8_t __c4, int8_t __c5, int8_t __c6, int8_t __c7, int8_t __c8, int8_t __c9, int8_t __c10,
    int8_t __c11, int8_t __c12, int8_t __c13, int8_t __c14, int8_t __c15) {
  return (v128_t)(__i8x16){__c0, __c1, __c2, __c3, __c4, __c5, __c6, __c7, __c8, __c9, __c10,
      __c11, __c12, __c13, __c14, __c15};
}
static __inline__ v128_t wasm_i16x8_make(int16_t __c0, int16_t __c1, int16_t __c2, int16_t __c3,
    int16_t __c4, int16_t __c5, int16_t __c6, int16_t __c7) {
  return (v128_t)(__i16x8){__c0, __c1, __c2, __c3, __c4, __c5, __c6, __c7};
}
static __inline__ v128_t wasm_i32x4_make(int32_t __c0, int32_t __c1, int32_t __c2, int32_t __c3) {
  return (v128_t)(__i32x4){__c0, __c1, __c2, __c3};
}
static __inline__ v128_t wasm_i64x2_make(int64_t __c0, int64_t __c1) {
  return (v128_t)(__i64x2){__c0, __c1};
}
static __inline__ v128_t wasm_u8x16_make(uint8_t __c0, uint8_t __c1, uint8_t __c2, uint8_t __c3,
    uint8_t __c4, uint8_t __c5, uint8_t __c6, uint8_t __c7, uint8_t __c8, uint8_t __c9,
    uint8_t __c10, uint8_t __c11, uint8_t __c12, uint8_t __c13, uint8_t __c14, uint8_t __c15) {
  return (v128_t)(__u8x16){__c0, __c1, __c2, __c3, __c4, __c5, __c6, __c7, __c8, __c9, __c10,
      __c11, __c12, __c13, __c14, __c15};
}
static __inline__ v128_t wasm_u16x8_make(uint16_t __c0, uint16_t __c1, uint16_t __c2, uint16_t __c3,
    uint16_t __c4, uint16_t __c5, uint16_t __c6, uint16_t __c7) {
  return (v128_t)(__u16x8){__c0, __c1, __c2, __c3, __c4, __c5, __c6, __c7};
}
static __inline__ v128_t wasm_u32x4_make(uint32_t __c0, uint32_t __c1, uint32_t __c2,
    uint32_t __c3) {
  return (v128_t)(__u32x4){__c0, __c1, __c2, __c3};
}
static __inline__ v128_t wasm_u64x2_make(uint64_t __c0, uint64_t __c1) {
  return (v128_t)(__u64x2){__c0, __c1};
}
static __inline__ v128_t wasm_f32x4_make(float __c0, float __c1, float __c2, float __c3) {
  return (v128_t)(__f32x4){__c0, __c1, __c2, __c3};
}
static __inline__ v128_t wasm_f64x2_make(double __c0, double __c1) {
  return (v128_t)(__f64x2){__c0, __c1};
}
static __inline__ v128_t wasm_i8x16_const(int8_t __c0, int8_t __c1, int8_t __c2, int8_t __c3,
    int8_t __c4, int8_t __c5, int8_t __c6, int8_t __c7, int8_t __c8, int8_t __c9, int8_t __c10,
    int8_t __c11, int8_t __c12, int8_t __c13, int8_t __c14, int8_t __c15) {
  return (v128_t)(__i8x16){__c0, __c1, __c2, __c3, __c4, __c5, __c6, __c7, __c8, __c9, __c10,
      __c11, __c12, __c13, __c14, __c15};
}
static __inline__ v128_t wasm_i16x8_const(int16_t __c0, int16_t __c1, int16_t __c2, int16_t __c3,
    int16_t __c4, int16_t __c5, int16_t __c6, int16_t __c7) {
  return (v128_t)(__i16x8){__c0, __c1, __c2, __c3, __c4, __c5, __c6, __c7};
}
static __inline__ v128_t wasm_i32x4_const(int32_t __c0, int32_t __c1, int32_t __c2, int32_t __c3) {
  return (v128_t)(__i32x4){__c0, __c1, __c2, __c3};
}
static __inline__ v128_t wasm_i64x2_const(int64_t __c0, int64_t __c1) {
  return (v128_t)(__i64x2){__c0, __c1};
}
static __inline__ v128_t wasm_u8x16_const(uint8_t __c0, uint8_t __c1, uint8_t __c2, uint8_t __c3,
    uint8_t __c4, uint8_t __c5, uint8_t __c6, uint8_t __c7, uint8_t __c8, uint8_t __c9,
    uint8_t __c10, uint8_t __c11, uint8_t __c12, uint8_t __c13, uint8_t __c14, uint8_t __c15) {
  return (v128_t)(__u8x16){__c0, __c1, __c2, __c3, __c4, __c5, __c6, __c7, __c8, __c9, __c10,
      __c11, __c12, __c13, __c14, __c15};
}
static __inline__ v128_t wasm_u16x8_const(uint16_t __c0, uint16_t __c1, uint16_t __c2,
    uint16_t __c3, uint16_t __c4, uint16_t __c5, uint16_t __c6, uint16_t __c7) {
  return (v128_t)(__u16x8){__c0, __c1, __c2, __c3, __c4, __c5, __c6, __c7};
}
static __inline__ v128_t wasm_u32x4_const(uint32_t __c0, uint32_t __c1, uint32_t __c2,
    uint32_t __c3) {
  return (v128_t)(__u32x4){__c0, __c1, __c2, __c3};
}
static __inline__ v128_t wasm_u64x2_const(uint64_t __c0, uint64_t __c1) {
  return (v128_t)(__u64x2){__c0, __c1};
}
static __inline__ v128_t wasm_f32x4_const(float __c0, float __c1, float __c2, float __c3) {
  return (v128_t)(__f32x4){__c0, __c1, __c2, __c3};
}
static __inline__ v128_t wasm_f64x2_const(double __c0, double __c1) {
  return (v128_t)(__f64x2){__c0, __c1};
}
static __inline__ v128_t wasm_i8x16_const_splat(int8_t __c) {
  return (v128_t)(__i8x16){__c, __c, __c, __c, __c, __c, __c, __c, __c, __c, __c, __c, __c, __c,
      __c, __c};
}
static __inline__ v128_t wasm_i16x8_const_splat(int16_t __c) {
  return (v128_t)(__i16x8){__c, __c, __c, __c, __c, __c, __c, __c};
}
static __inline__ v128_t wasm_i32x4_const_splat(int32_t __c) {
  return (v128_t)(__i32x4){__c, __c, __c, __c};
}
static __inline__ v128_t wasm_i64x2_const_splat(int64_t __c) {
  return (v128_t)(__i64x2){__c, __c};
}
static __inline__ v128_t wasm_u8x16_const_splat(uint8_t __c) {
  return (v128_t)(__u8x16){__c, __c, __c, __c, __c, __c, __c, __c, __c, __c, __c, __c, __c, __c,
      __c, __c};
}
static __inline__ v128_t wasm_u16x8_const_splat(uint16_t __c) {
  return (v128_t)(__u16x8){__c, __c, __c, __c, __c, __c, __c, __c};
}
static __inline__ v128_t wasm_u32x4_const_splat(uint32_t __c) {
  return (v128_t)(__u32x4){__c, __c, __c, __c};
}
static __inline__ v128_t wasm_u64x2_const_splat(uint64_t __c) {
  return (v128_t)(__u64x2){__c, __c};
}
static __inline__ v128_t wasm_f32x4_const_splat(float __c) {
  return (v128_t)(__f32x4){__c, __c, __c, __c};
}
static __inline__ v128_t wasm_f64x2_const_splat(double __c) {
  return (v128_t)(__f64x2){__c, __c};
}
static __inline__ v128_t wasm_i8x16_splat(int8_t __a) {
  return (v128_t)(__i8x16){__a, __a, __a, __a, __a, __a, __a, __a, __a, __a, __a, __a, __a, __a,
      __a, __a};
}
static __inline__ v128_t wasm_i16x8_splat(int16_t __a) {
  return (v128_t)(__i16x8){__a, __a, __a, __a, __a, __a, __a, __a};
}
static __inline__ v128_t wasm_i32x4_splat(int32_t __a) {
  return (v128_t)(__i32x4){__a, __a, __a, __a};
}
static __inline__ v128_t wasm_i64x2_splat(int64_t __a) {
  return (v128_t)(__i64x2){__a, __a};
}
static __inline__ v128_t wasm_u8x16_splat(uint8_t __a) {
  return (v128_t)(__u8x16){__a, __a, __a, __a, __a, __a, __a, __a, __a, __a, __a, __a, __a, __a,
      __a, __a};
}
static __inline__ v128_t wasm_u16x8_splat(uint16_t __a) {
  return (v128_t)(__u16x8){__a, __a, __a, __a, __a, __a, __a, __a};
}
static __inline__ v128_t wasm_u32x4_splat(uint32_t __a) {
  return (v128_t)(__u32x4){__a, __a, __a, __a};
}
static __inline__ v128_t wasm_u64x2_splat(uint64_t __a) {
  return (v128_t)(__u64x2){__a, __a};
}
static __inline__ v128_t wasm_f32x4_splat(float __a) {
  return (v128_t)(__f32x4){__a, __a, __a, __a};
}
static __inline__ v128_t wasm_f64x2_splat(double __a) {
  return (v128_t)(__f64x2){__a, __a};
}
static __inline__ int8_t wasm_i8x16_extract_lane(v128_t __a, int __i) {
  __i8x16 __x = (__i8x16)__a;
  return __x[__i];
}
static __inline__ int16_t wasm_i16x8_extract_lane(v128_t __a, int __i) {
  __i16x8 __x = (__i16x8)__a;
  return __x[__i];
}
static __inline__ int32_t wasm_i32x4_extract_lane(v128_t __a, int __i) {
  __i32x4 __x = (__i32x4)__a;
  return __x[__i];
}
static __inline__ int64_t wasm_i64x2_extract_lane(v128_t __a, int __i) {
  __i64x2 __x = (__i64x2)__a;
  return __x[__i];
}
static __inline__ uint8_t wasm_u8x16_extract_lane(v128_t __a, int __i) {
  __u8x16 __x = (__u8x16)__a;
  return __x[__i];
}
static __inline__ uint16_t wasm_u16x8_extract_lane(v128_t __a, int __i) {
  __u16x8 __x = (__u16x8)__a;
  return __x[__i];
}
static __inline__ uint32_t wasm_u32x4_extract_lane(v128_t __a, int __i) {
  __u32x4 __x = (__u32x4)__a;
  return __x[__i];
}
static __inline__ uint64_t wasm_u64x2_extract_lane(v128_t __a, int __i) {
  __u64x2 __x = (__u64x2)__a;
  return __x[__i];
}
static __inline__ float wasm_f32x4_extract_lane(v128_t __a, int __i) {
  __f32x4 __x = (__f32x4)__a;
  return __x[__i];
}
static __inline__ double wasm_f64x2_extract_lane(v128_t __a, int __i) {
  __f64x2 __x = (__f64x2)__a;
  return __x[__i];
}
static __inline__ v128_t wasm_i8x16_replace_lane(v128_t __a, int __i, int8_t __b) {
  __i8x16 __r = (__i8x16)__a;
  __r[__i] = __b;
  return (v128_t)__r;
}
static __inline__ v128_t wasm_i16x8_replace_lane(v128_t __a, int __i, int16_t __b) {
  __i16x8 __r = (__i16x8)__a;
  __r[__i] = __b;
  return (v128_t)__r;
}
static __inline__ v128_t wasm_i32x4_replace_lane(v128_t __a, int __i, int32_t __b) {
  __i32x4 __r = (__i32x4)__a;
  __r[__i] = __b;
  return (v128_t)__r;
}
static __inline__ v128_t wasm_i64x2_replace_lane(v128_t __a, int __i, int64_t __b) {
  __i64x2 __r = (__i64x2)__a;
  __r[__i] = __b;
  return (v128_t)__r;
}
static __inline__ v128_t wasm_u8x16_replace_lane(v128_t __a, int __i, uint8_t __b) {
  __u8x16 __r = (__u8x16)__a;
  __r[__i] = __b;
  return (v128_t)__r;
}
static __inline__ v128_t wasm_u16x8_replace_lane(v128_t __a, int __i, uint16_t __b) {
  __u16x8 __r = (__u16x8)__a;
  __r[__i] = __b;
  return (v128_t)__r;
}
static __inline__ v128_t wasm_u32x4_replace_lane(v128_t __a, int __i, uint32_t __b) {
  __u32x4 __r = (__u32x4)__a;
  __r[__i] = __b;
  return (v128_t)__r;
}
static __inline__ v128_t wasm_u64x2_replace_lane(v128_t __a, int __i, uint64_t __b) {
  __u64x2 __r = (__u64x2)__a;
  __r[__i] = __b;
  return (v128_t)__r;
}
static __inline__ v128_t wasm_f32x4_replace_lane(v128_t __a, int __i, float __b) {
  __f32x4 __r = (__f32x4)__a;
  __r[__i] = __b;
  return (v128_t)__r;
}
static __inline__ v128_t wasm_f64x2_replace_lane(v128_t __a, int __i, double __b) {
  __f64x2 __r = (__f64x2)__a;
  __r[__i] = __b;
  return (v128_t)__r;
}

/* Integer arithmetic. The lanes that wrap are unsigned, where C has them wrap too. */
static __inline__ v128_t wasm_i8x16_add(v128_t __a, v128_t __b) {
  return (v128_t)((__u8x16)__a + (__u8x16)__b);
}
static __inline__ v128_t wasm_i8x16_sub(v128_t __a, v128_t __b) {
  return (v128_t)((__u8x16)__a - (__u8x16)__b);
}
static __inline__ v128_t wasm_i8x16_neg(v128_t __a) {
  return (v128_t)(-(__u8x16)__a);
}
static __inline__ v128_t wasm_i8x16_abs(v128_t __a) {
  __i8x16 __x = (__i8x16)__a;
  __u8x16 __y = (__u8x16)__a;
  __u8x16 __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __x[__i] < 0 ? -__y[__i] : __y[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_i8x16_shl(v128_t __a, uint32_t __b) {
  return (v128_t)((__u8x16)__a << (__b & 7));
}
static __inline__ v128_t wasm_i8x16_shr(v128_t __a, uint32_t __b) {
  return (v128_t)((__i8x16)__a >> (__b & 7));
}
static __inline__ v128_t wasm_u8x16_shr(v128_t __a, uint32_t __b) {
  return (v128_t)((__u8x16)__a >> (__b & 7));
}
static __inline__ bool wasm_i8x16_all_true(v128_t __a) {
  __i8x16 __x = (__i8x16)__a;
  for (int __i = 0; __i < 16; __i++) if (__x[__i] == 0) return false;
  return true;
}
static __inline__ uint32_t wasm_i8x16_bitmask(v128_t __a) {
  __i8x16 __x = (__i8x16)__a;
  uint32_t __m = 0;
  for (int __i = 0; __i < 16; __i++) __m |= (uint32_t)(__x[__i] < 0) << __i;
  return __m;
}
static __inline__ v128_t wasm_i16x8_add(v128_t __a, v128_t __b) {
  return (v128_t)((__u16x8)__a + (__u16x8)__b);
}
static __inline__ v128_t wasm_i16x8_sub(v128_t __a, v128_t __b) {
  return (v128_t)((__u16x8)__a - (__u16x8)__b);
}
static __inline__ v128_t wasm_i16x8_mul(v128_t __a, v128_t __b) {
  return (v128_t)((__u16x8)__a * (__u16x8)__b);
}
static __inline__ v128_t wasm_i16x8_neg(v128_t __a) {
  return (v128_t)(-(__u16x8)__a);
}
static __inline__ v128_t wasm_i16x8_abs(v128_t __a) {
  __i16x8 __x = (__i16x8)__a;
  __u16x8 __y = (__u16x8)__a;
  __u16x8 __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __x[__i] < 0 ? -__y[__i] : __y[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_i16x8_shl(v128_t __a, uint32_t __b) {
  return (v128_t)((__u16x8)__a << (__b & 15));
}
static __inline__ v128_t wasm_i16x8_shr(v128_t __a, uint32_t __b) {
  return (v128_t)((__i16x8)__a >> (__b & 15));
}
static __inline__ v128_t wasm_u16x8_shr(v128_t __a, uint32_t __b) {
  return (v128_t)((__u16x8)__a >> (__b & 15));
}
static __inline__ bool wasm_i16x8_all_true(v128_t __a) {
  __i16x8 __x = (__i16x8)__a;
  for (int __i = 0; __i < 8; __i++) if (__x[__i] == 0) return false;
  return true;
}
static __inline__ uint32_t wasm_i16x8_bitmask(v128_t __a) {
  __i16x8 __x = (__i16x8)__a;
  uint32_t __m = 0;
  for (int __i = 0; __i < 8; __i++) __m |= (uint32_t)(__x[__i] < 0) << __i;
  return __m;
}
static __inline__ v128_t wasm_i32x4_add(v128_t __a, v128_t __b) {
  return (v128_t)((__u32x4)__a + (__u32x4)__b);
}
static __inline__ v128_t wasm_i32x4_sub(v128_t __a, v128_t __b) {
  return (v128_t)((__u32x4)__a - (__u32x4)__b);
}
static __inline__ v128_t wasm_i32x4_mul(v128_t __a, v128_t __b) {
  return (v128_t)((__u32x4)__a * (__u32x4)__b);
}
static __inline__ v128_t wasm_i32x4_neg(v128_t __a) {
  return (v128_t)(-(__u32x4)__a);
}
static __inline__ v128_t wasm_i32x4_abs(v128_t __a) {
  __i32x4 __x = (__i32x4)__a;
  __u32x4 __y = (__u32x4)__a;
  __u32x4 __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __x[__i] < 0 ? -__y[__i] : __y[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_i32x4_shl(v128_t __a, uint32_t __b) {
  return (v128_t)((__u32x4)__a << (__b & 31));
}
static __inline__ v128_t wasm_i32x4_shr(v128_t __a, uint32_t __b) {
  return (v128_t)((__i32x4)__a >> (__b & 31));
}
static __inline__ v128_t wasm_u32x4_shr(v128_t __a, uint32_t __b) {
  return (v128_t)((__u32x4)__a >> (__b & 31));
}
static __inline__ bool wasm_i32x4_all_true(v128_t __a) {
  __i32x4 __x = (__i32x4)__a;
  for (int __i = 0; __i < 4; __i++) if (__x[__i] == 0) return false;
  return true;
}
static __inline__ uint32_t wasm_i32x4_bitmask(v128_t __a) {
  __i32x4 __x = (__i32x4)__a;
  uint32_t __m = 0;
  for (int __i = 0; __i < 4; __i++) __m |= (uint32_t)(__x[__i] < 0) << __i;
  return __m;
}
static __inline__ v128_t wasm_i64x2_add(v128_t __a, v128_t __b) {
  return (v128_t)((__u64x2)__a + (__u64x2)__b);
}
static __inline__ v128_t wasm_i64x2_sub(v128_t __a, v128_t __b) {
  return (v128_t)((__u64x2)__a - (__u64x2)__b);
}
static __inline__ v128_t wasm_i64x2_mul(v128_t __a, v128_t __b) {
  return (v128_t)((__u64x2)__a * (__u64x2)__b);
}
static __inline__ v128_t wasm_i64x2_neg(v128_t __a) {
  return (v128_t)(-(__u64x2)__a);
}
static __inline__ v128_t wasm_i64x2_abs(v128_t __a) {
  __i64x2 __x = (__i64x2)__a;
  __u64x2 __y = (__u64x2)__a;
  __u64x2 __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __x[__i] < 0 ? -__y[__i] : __y[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_i64x2_shl(v128_t __a, uint32_t __b) {
  return (v128_t)((__u64x2)__a << (__b & 63));
}
static __inline__ v128_t wasm_i64x2_shr(v128_t __a, uint32_t __b) {
  return (v128_t)((__i64x2)__a >> (__b & 63));
}
static __inline__ v128_t wasm_u64x2_shr(v128_t __a, uint32_t __b) {
  return (v128_t)((__u64x2)__a >> (__b & 63));
}
static __inline__ bool wasm_i64x2_all_true(v128_t __a) {
  __i64x2 __x = (__i64x2)__a;
  for (int __i = 0; __i < 2; __i++) if (__x[__i] == 0) return false;
  return true;
}
static __inline__ uint32_t wasm_i64x2_bitmask(v128_t __a) {
  __i64x2 __x = (__i64x2)__a;
  uint32_t __m = 0;
  for (int __i = 0; __i < 2; __i++) __m |= (uint32_t)(__x[__i] < 0) << __i;
  return __m;
}
static __inline__ v128_t wasm_i8x16_popcnt(v128_t __a) {
  __u8x16 __x = (__u8x16)__a;
  __u8x16 __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __builtin_popcount(__x[__i]);
  return (v128_t)__r;
}
static __inline__ v128_t wasm_i8x16_add_sat(v128_t __a, v128_t __b) {
  __i8x16 __x = (__i8x16)__a;
  __i8x16 __y = (__i8x16)__b;
  __i8x16 __r;
  for (int __i = 0; __i < 16; __i++)
    { int __s = __x[__i] + __y[__i]; __r[__i] = __s < -128 ? -128 : __s > 127 ? 127
        : __s; } return (v128_t)__r;
}
static __inline__ v128_t wasm_i8x16_sub_sat(v128_t __a, v128_t __b) {
  __i8x16 __x = (__i8x16)__a;
  __i8x16 __y = (__i8x16)__b;
  __i8x16 __r;
  for (int __i = 0; __i < 16; __i++)
    { int __s = __x[__i] - __y[__i]; __r[__i] = __s < -128 ? -128 : __s > 127 ? 127
        : __s; } return (v128_t)__r;
}
static __inline__ v128_t wasm_u8x16_add_sat(v128_t __a, v128_t __b) {
  __u8x16 __x = (__u8x16)__a;
  __u8x16 __y = (__u8x16)__b;
  __u8x16 __r;
  for (int __i = 0; __i < 16; __i++)
    { int __s = __x[__i] + __y[__i]; __r[__i] = __s < 0 ? 0 : __s > 255 ? 255
        : __s; } return (v128_t)__r;
}
static __inline__ v128_t wasm_u8x16_sub_sat(v128_t __a, v128_t __b) {
  __u8x16 __x = (__u8x16)__a;
  __u8x16 __y = (__u8x16)__b;
  __u8x16 __r;
  for (int __i = 0; __i < 16; __i++)
    { int __s = __x[__i] - __y[__i]; __r[__i] = __s < 0 ? 0 : __s > 255 ? 255
        : __s; } return (v128_t)__r;
}
static __inline__ v128_t wasm_i16x8_add_sat(v128_t __a, v128_t __b) {
  __i16x8 __x = (__i16x8)__a;
  __i16x8 __y = (__i16x8)__b;
  __i16x8 __r;
  for (int __i = 0; __i < 8; __i++)
    { int __s = __x[__i] + __y[__i]; __r[__i] = __s < -32768 ? -32768 : __s > 32767 ? 32767
        : __s; } return (v128_t)__r;
}
static __inline__ v128_t wasm_i16x8_sub_sat(v128_t __a, v128_t __b) {
  __i16x8 __x = (__i16x8)__a;
  __i16x8 __y = (__i16x8)__b;
  __i16x8 __r;
  for (int __i = 0; __i < 8; __i++)
    { int __s = __x[__i] - __y[__i]; __r[__i] = __s < -32768 ? -32768 : __s > 32767 ? 32767
        : __s; } return (v128_t)__r;
}
static __inline__ v128_t wasm_u16x8_add_sat(v128_t __a, v128_t __b) {
  __u16x8 __x = (__u16x8)__a;
  __u16x8 __y = (__u16x8)__b;
  __u16x8 __r;
  for (int __i = 0; __i < 8; __i++)
    { int __s = __x[__i] + __y[__i]; __r[__i] = __s < 0 ? 0 : __s > 65535 ? 65535
        : __s; } return (v128_t)__r;
}
static __inline__ v128_t wasm_u16x8_sub_sat(v128_t __a, v128_t __b) {
  __u16x8 __x = (__u16x8)__a;
  __u16x8 __y = (__u16x8)__b;
  __u16x8 __r;
  for (int __i = 0; __i < 8; __i++)
    { int __s = __x[__i] - __y[__i]; __r[__i] = __s < 0 ? 0 : __s > 65535 ? 65535
        : __s; } return (v128_t)__r;
}
static __inline__ v128_t wasm_i8x16_min(v128_t __a, v128_t __b) {
  __i8x16 __x = (__i8x16)__a;
  __i8x16 __y = (__i8x16)__b;
  __i8x16 __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __x[__i] < __y[__i] ? __x[__i] : __y[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_i8x16_max(v128_t __a, v128_t __b) {
  __i8x16 __x = (__i8x16)__a;
  __i8x16 __y = (__i8x16)__b;
  __i8x16 __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __x[__i] < __y[__i] ? __y[__i] : __x[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_u8x16_min(v128_t __a, v128_t __b) {
  __u8x16 __x = (__u8x16)__a;
  __u8x16 __y = (__u8x16)__b;
  __u8x16 __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __x[__i] < __y[__i] ? __x[__i] : __y[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_u8x16_max(v128_t __a, v128_t __b) {
  __u8x16 __x = (__u8x16)__a;
  __u8x16 __y = (__u8x16)__b;
  __u8x16 __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __x[__i] < __y[__i] ? __y[__i] : __x[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_i16x8_min(v128_t __a, v128_t __b) {
  __i16x8 __x = (__i16x8)__a;
  __i16x8 __y = (__i16x8)__b;
  __i16x8 __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __x[__i] < __y[__i] ? __x[__i] : __y[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_i16x8_max(v128_t __a, v128_t __b) {
  __i16x8 __x = (__i16x8)__a;
  __i16x8 __y = (__i16x8)__b;
  __i16x8 __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __x[__i] < __y[__i] ? __y[__i] : __x[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_u16x8_min(v128_t __a, v128_t __b) {
  __u16x8 __x = (__u16x8)__a;
  __u16x8 __y = (__u16x8)__b;
  __u16x8 __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __x[__i] < __y[__i] ? __x[__i] : __y[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_u16x8_max(v128_t __a, v128_t __b) {
  __u16x8 __x = (__u16x8)__a;
  __u16x8 __y = (__u16x8)__b;
  __u16x8 __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __x[__i] < __y[__i] ? __y[__i] : __x[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_i32x4_min(v128_t __a, v128_t __b) {
  __i32x4 __x = (__i32x4)__a;
  __i32x4 __y = (__i32x4)__b;
  __i32x4 __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __x[__i] < __y[__i] ? __x[__i] : __y[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_i32x4_max(v128_t __a, v128_t __b) {
  __i32x4 __x = (__i32x4)__a;
  __i32x4 __y = (__i32x4)__b;
  __i32x4 __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __x[__i] < __y[__i] ? __y[__i] : __x[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_u32x4_min(v128_t __a, v128_t __b) {
  __u32x4 __x = (__u32x4)__a;
  __u32x4 __y = (__u32x4)__b;
  __u32x4 __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __x[__i] < __y[__i] ? __x[__i] : __y[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_u32x4_max(v128_t __a, v128_t __b) {
  __u32x4 __x = (__u32x4)__a;
  __u32x4 __y = (__u32x4)__b;
  __u32x4 __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __x[__i] < __y[__i] ? __y[__i] : __x[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_u8x16_avgr(v128_t __a, v128_t __b) {
  __u8x16 __x = (__u8x16)__a;
  __u8x16 __y = (__u8x16)__b;
  __u8x16 __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = (__x[__i] + __y[__i] + 1) >> 1;
  return (v128_t)__r;
}
static __inline__ v128_t wasm_u16x8_avgr(v128_t __a, v128_t __b) {
  __u16x8 __x = (__u16x8)__a;
  __u16x8 __y = (__u16x8)__b;
  __u16x8 __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (__x[__i] + __y[__i] + 1) >> 1;
  return (v128_t)__r;
}
static __inline__ v128_t wasm_i16x8_q15mulr_sat(v128_t __a, v128_t __b) {
  __i16x8 __x = (__i16x8)__a;
  __i16x8 __y = (__i16x8)__b;
  int __s;
  __i16x8 __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (__s = (__x[__i] * __y[__i] + 0x4000) >> 15) > 32767 ? 32767 : __s;
  return (v128_t)__r;
}
static __inline__ v128_t wasm_i32x4_dot_i16x8(v128_t __a, v128_t __b) {
  __i16x8 __x = (__i16x8)__a;
  __i16x8 __y = (__i16x8)__b;
  __u32x4 __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (unsigned)(__x[2 * __i] * __y[2 * __i]) + (unsigned)(__x[2 * __i
        + 1] * __y[2 * __i + 1]);
  return (v128_t)__r;
}

/* Comparisons. A lane that holds is all ones and a lane that does not is zero. */
static __inline__ v128_t wasm_i8x16_eq(v128_t __a, v128_t __b) {
  return (v128_t)((__i8x16)__a == (__i8x16)__b);
}
static __inline__ v128_t wasm_i8x16_ne(v128_t __a, v128_t __b) {
  return (v128_t)((__i8x16)__a != (__i8x16)__b);
}
static __inline__ v128_t wasm_i8x16_lt(v128_t __a, v128_t __b) {
  return (v128_t)((__i8x16)__a < (__i8x16)__b);
}
static __inline__ v128_t wasm_i8x16_gt(v128_t __a, v128_t __b) {
  return (v128_t)((__i8x16)__a > (__i8x16)__b);
}
static __inline__ v128_t wasm_i8x16_le(v128_t __a, v128_t __b) {
  return (v128_t)((__i8x16)__a <= (__i8x16)__b);
}
static __inline__ v128_t wasm_i8x16_ge(v128_t __a, v128_t __b) {
  return (v128_t)((__i8x16)__a >= (__i8x16)__b);
}
static __inline__ v128_t wasm_i16x8_eq(v128_t __a, v128_t __b) {
  return (v128_t)((__i16x8)__a == (__i16x8)__b);
}
static __inline__ v128_t wasm_i16x8_ne(v128_t __a, v128_t __b) {
  return (v128_t)((__i16x8)__a != (__i16x8)__b);
}
static __inline__ v128_t wasm_i16x8_lt(v128_t __a, v128_t __b) {
  return (v128_t)((__i16x8)__a < (__i16x8)__b);
}
static __inline__ v128_t wasm_i16x8_gt(v128_t __a, v128_t __b) {
  return (v128_t)((__i16x8)__a > (__i16x8)__b);
}
static __inline__ v128_t wasm_i16x8_le(v128_t __a, v128_t __b) {
  return (v128_t)((__i16x8)__a <= (__i16x8)__b);
}
static __inline__ v128_t wasm_i16x8_ge(v128_t __a, v128_t __b) {
  return (v128_t)((__i16x8)__a >= (__i16x8)__b);
}
static __inline__ v128_t wasm_i32x4_eq(v128_t __a, v128_t __b) {
  return (v128_t)((__i32x4)__a == (__i32x4)__b);
}
static __inline__ v128_t wasm_i32x4_ne(v128_t __a, v128_t __b) {
  return (v128_t)((__i32x4)__a != (__i32x4)__b);
}
static __inline__ v128_t wasm_i32x4_lt(v128_t __a, v128_t __b) {
  return (v128_t)((__i32x4)__a < (__i32x4)__b);
}
static __inline__ v128_t wasm_i32x4_gt(v128_t __a, v128_t __b) {
  return (v128_t)((__i32x4)__a > (__i32x4)__b);
}
static __inline__ v128_t wasm_i32x4_le(v128_t __a, v128_t __b) {
  return (v128_t)((__i32x4)__a <= (__i32x4)__b);
}
static __inline__ v128_t wasm_i32x4_ge(v128_t __a, v128_t __b) {
  return (v128_t)((__i32x4)__a >= (__i32x4)__b);
}
static __inline__ v128_t wasm_i64x2_eq(v128_t __a, v128_t __b) {
  return (v128_t)((__i64x2)__a == (__i64x2)__b);
}
static __inline__ v128_t wasm_i64x2_ne(v128_t __a, v128_t __b) {
  return (v128_t)((__i64x2)__a != (__i64x2)__b);
}
static __inline__ v128_t wasm_i64x2_lt(v128_t __a, v128_t __b) {
  return (v128_t)((__i64x2)__a < (__i64x2)__b);
}
static __inline__ v128_t wasm_i64x2_gt(v128_t __a, v128_t __b) {
  return (v128_t)((__i64x2)__a > (__i64x2)__b);
}
static __inline__ v128_t wasm_i64x2_le(v128_t __a, v128_t __b) {
  return (v128_t)((__i64x2)__a <= (__i64x2)__b);
}
static __inline__ v128_t wasm_i64x2_ge(v128_t __a, v128_t __b) {
  return (v128_t)((__i64x2)__a >= (__i64x2)__b);
}
static __inline__ v128_t wasm_u8x16_lt(v128_t __a, v128_t __b) {
  return (v128_t)((__u8x16)__a < (__u8x16)__b);
}
static __inline__ v128_t wasm_u8x16_gt(v128_t __a, v128_t __b) {
  return (v128_t)((__u8x16)__a > (__u8x16)__b);
}
static __inline__ v128_t wasm_u8x16_le(v128_t __a, v128_t __b) {
  return (v128_t)((__u8x16)__a <= (__u8x16)__b);
}
static __inline__ v128_t wasm_u8x16_ge(v128_t __a, v128_t __b) {
  return (v128_t)((__u8x16)__a >= (__u8x16)__b);
}
static __inline__ v128_t wasm_u16x8_lt(v128_t __a, v128_t __b) {
  return (v128_t)((__u16x8)__a < (__u16x8)__b);
}
static __inline__ v128_t wasm_u16x8_gt(v128_t __a, v128_t __b) {
  return (v128_t)((__u16x8)__a > (__u16x8)__b);
}
static __inline__ v128_t wasm_u16x8_le(v128_t __a, v128_t __b) {
  return (v128_t)((__u16x8)__a <= (__u16x8)__b);
}
static __inline__ v128_t wasm_u16x8_ge(v128_t __a, v128_t __b) {
  return (v128_t)((__u16x8)__a >= (__u16x8)__b);
}
static __inline__ v128_t wasm_u32x4_lt(v128_t __a, v128_t __b) {
  return (v128_t)((__u32x4)__a < (__u32x4)__b);
}
static __inline__ v128_t wasm_u32x4_gt(v128_t __a, v128_t __b) {
  return (v128_t)((__u32x4)__a > (__u32x4)__b);
}
static __inline__ v128_t wasm_u32x4_le(v128_t __a, v128_t __b) {
  return (v128_t)((__u32x4)__a <= (__u32x4)__b);
}
static __inline__ v128_t wasm_u32x4_ge(v128_t __a, v128_t __b) {
  return (v128_t)((__u32x4)__a >= (__u32x4)__b);
}
static __inline__ v128_t wasm_f32x4_eq(v128_t __a, v128_t __b) {
  return (v128_t)((__f32x4)__a == (__f32x4)__b);
}
static __inline__ v128_t wasm_f32x4_ne(v128_t __a, v128_t __b) {
  return (v128_t)((__f32x4)__a != (__f32x4)__b);
}
static __inline__ v128_t wasm_f32x4_lt(v128_t __a, v128_t __b) {
  return (v128_t)((__f32x4)__a < (__f32x4)__b);
}
static __inline__ v128_t wasm_f32x4_gt(v128_t __a, v128_t __b) {
  return (v128_t)((__f32x4)__a > (__f32x4)__b);
}
static __inline__ v128_t wasm_f32x4_le(v128_t __a, v128_t __b) {
  return (v128_t)((__f32x4)__a <= (__f32x4)__b);
}
static __inline__ v128_t wasm_f32x4_ge(v128_t __a, v128_t __b) {
  return (v128_t)((__f32x4)__a >= (__f32x4)__b);
}
static __inline__ v128_t wasm_f64x2_eq(v128_t __a, v128_t __b) {
  return (v128_t)((__f64x2)__a == (__f64x2)__b);
}
static __inline__ v128_t wasm_f64x2_ne(v128_t __a, v128_t __b) {
  return (v128_t)((__f64x2)__a != (__f64x2)__b);
}
static __inline__ v128_t wasm_f64x2_lt(v128_t __a, v128_t __b) {
  return (v128_t)((__f64x2)__a < (__f64x2)__b);
}
static __inline__ v128_t wasm_f64x2_gt(v128_t __a, v128_t __b) {
  return (v128_t)((__f64x2)__a > (__f64x2)__b);
}
static __inline__ v128_t wasm_f64x2_le(v128_t __a, v128_t __b) {
  return (v128_t)((__f64x2)__a <= (__f64x2)__b);
}
static __inline__ v128_t wasm_f64x2_ge(v128_t __a, v128_t __b) {
  return (v128_t)((__f64x2)__a >= (__f64x2)__b);
}

/* Float arithmetic. */
static __inline__ v128_t wasm_f32x4_abs(v128_t __a) {
  return (v128_t)((__u32x4)__a & ~0x80000000u);
}
static __inline__ v128_t wasm_f32x4_neg(v128_t __a) {
  return (v128_t)((__u32x4)__a ^ 0x80000000u);
}
static __inline__ v128_t wasm_f32x4_sqrt(v128_t __a) {
  __f32x4 __x = (__f32x4)__a;
  __f32x4 __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __builtin_sqrtf(__x[__i]);
  return (v128_t)__r;
}
static __inline__ v128_t wasm_f32x4_ceil(v128_t __a) {
  __f32x4 __x = (__f32x4)__a;
  __f32x4 __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __rucc_wasm_ceil_f32(__x[__i]);
  return (v128_t)__r;
}
static __inline__ v128_t wasm_f32x4_floor(v128_t __a) {
  __f32x4 __x = (__f32x4)__a;
  __f32x4 __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __rucc_wasm_floor_f32(__x[__i]);
  return (v128_t)__r;
}
static __inline__ v128_t wasm_f32x4_trunc(v128_t __a) {
  __f32x4 __x = (__f32x4)__a;
  __f32x4 __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __rucc_wasm_trunc_f32(__x[__i]);
  return (v128_t)__r;
}
static __inline__ v128_t wasm_f32x4_nearest(v128_t __a) {
  __f32x4 __x = (__f32x4)__a;
  __f32x4 __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __rucc_wasm_nearest_f32(__x[__i]);
  return (v128_t)__r;
}
static __inline__ v128_t wasm_f32x4_add(v128_t __a, v128_t __b) {
  return (v128_t)((__f32x4)__a + (__f32x4)__b);
}
static __inline__ v128_t wasm_f32x4_sub(v128_t __a, v128_t __b) {
  return (v128_t)((__f32x4)__a - (__f32x4)__b);
}
static __inline__ v128_t wasm_f32x4_mul(v128_t __a, v128_t __b) {
  return (v128_t)((__f32x4)__a * (__f32x4)__b);
}
static __inline__ v128_t wasm_f32x4_div(v128_t __a, v128_t __b) {
  return (v128_t)((__f32x4)__a / (__f32x4)__b);
}
static __inline__ v128_t wasm_f32x4_min(v128_t __a, v128_t __b) {
  __f32x4 __x = (__f32x4)__a;
  __f32x4 __y = (__f32x4)__b;
  __f32x4 __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __builtin_wasm_min_f32(__x[__i], __y[__i]);
  return (v128_t)__r;
}
static __inline__ v128_t wasm_f32x4_max(v128_t __a, v128_t __b) {
  __f32x4 __x = (__f32x4)__a;
  __f32x4 __y = (__f32x4)__b;
  __f32x4 __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __builtin_wasm_max_f32(__x[__i], __y[__i]);
  return (v128_t)__r;
}
static __inline__ v128_t wasm_f32x4_pmin(v128_t __a, v128_t __b) {
  __f32x4 __x = (__f32x4)__a;
  __f32x4 __y = (__f32x4)__b;
  __f32x4 __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __y[__i] < __x[__i] ? __y[__i] : __x[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_f32x4_pmax(v128_t __a, v128_t __b) {
  __f32x4 __x = (__f32x4)__a;
  __f32x4 __y = (__f32x4)__b;
  __f32x4 __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __x[__i] < __y[__i] ? __y[__i] : __x[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_f64x2_abs(v128_t __a) {
  return (v128_t)((__u64x2)__a & ~0x8000000000000000ull);
}
static __inline__ v128_t wasm_f64x2_neg(v128_t __a) {
  return (v128_t)((__u64x2)__a ^ 0x8000000000000000ull);
}
static __inline__ v128_t wasm_f64x2_sqrt(v128_t __a) {
  __f64x2 __x = (__f64x2)__a;
  __f64x2 __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __builtin_sqrt(__x[__i]);
  return (v128_t)__r;
}
static __inline__ v128_t wasm_f64x2_ceil(v128_t __a) {
  __f64x2 __x = (__f64x2)__a;
  __f64x2 __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __rucc_wasm_ceil_f64(__x[__i]);
  return (v128_t)__r;
}
static __inline__ v128_t wasm_f64x2_floor(v128_t __a) {
  __f64x2 __x = (__f64x2)__a;
  __f64x2 __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __rucc_wasm_floor_f64(__x[__i]);
  return (v128_t)__r;
}
static __inline__ v128_t wasm_f64x2_trunc(v128_t __a) {
  __f64x2 __x = (__f64x2)__a;
  __f64x2 __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __rucc_wasm_trunc_f64(__x[__i]);
  return (v128_t)__r;
}
static __inline__ v128_t wasm_f64x2_nearest(v128_t __a) {
  __f64x2 __x = (__f64x2)__a;
  __f64x2 __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __rucc_wasm_nearest_f64(__x[__i]);
  return (v128_t)__r;
}
static __inline__ v128_t wasm_f64x2_add(v128_t __a, v128_t __b) {
  return (v128_t)((__f64x2)__a + (__f64x2)__b);
}
static __inline__ v128_t wasm_f64x2_sub(v128_t __a, v128_t __b) {
  return (v128_t)((__f64x2)__a - (__f64x2)__b);
}
static __inline__ v128_t wasm_f64x2_mul(v128_t __a, v128_t __b) {
  return (v128_t)((__f64x2)__a * (__f64x2)__b);
}
static __inline__ v128_t wasm_f64x2_div(v128_t __a, v128_t __b) {
  return (v128_t)((__f64x2)__a / (__f64x2)__b);
}
static __inline__ v128_t wasm_f64x2_min(v128_t __a, v128_t __b) {
  __f64x2 __x = (__f64x2)__a;
  __f64x2 __y = (__f64x2)__b;
  __f64x2 __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __builtin_wasm_min_f64(__x[__i], __y[__i]);
  return (v128_t)__r;
}
static __inline__ v128_t wasm_f64x2_max(v128_t __a, v128_t __b) {
  __f64x2 __x = (__f64x2)__a;
  __f64x2 __y = (__f64x2)__b;
  __f64x2 __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __builtin_wasm_max_f64(__x[__i], __y[__i]);
  return (v128_t)__r;
}
static __inline__ v128_t wasm_f64x2_pmin(v128_t __a, v128_t __b) {
  __f64x2 __x = (__f64x2)__a;
  __f64x2 __y = (__f64x2)__b;
  __f64x2 __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __y[__i] < __x[__i] ? __y[__i] : __x[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_f64x2_pmax(v128_t __a, v128_t __b) {
  __f64x2 __x = (__f64x2)__a;
  __f64x2 __y = (__f64x2)__b;
  __f64x2 __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __x[__i] < __y[__i] ? __y[__i] : __x[__i];
  return (v128_t)__r;
}

/* Conversions, narrowing and widening. */
static __inline__ v128_t wasm_f32x4_convert_i32x4(v128_t __a) {
  __i32x4 __x = (__i32x4)__a;
  __f32x4 __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __x[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_f32x4_convert_u32x4(v128_t __a) {
  __u32x4 __x = (__u32x4)__a;
  __f32x4 __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __x[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_f64x2_convert_low_i32x4(v128_t __a) {
  __i32x4 __x = (__i32x4)__a;
  __f64x2 __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __x[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_f64x2_convert_low_u32x4(v128_t __a) {
  __u32x4 __x = (__u32x4)__a;
  __f64x2 __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __x[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_i32x4_trunc_sat_f32x4(v128_t __a) {
  __f32x4 __x = (__f32x4)__a;
  __i32x4 __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __builtin_wasm_trunc_saturate_s_i32_f32(__x[__i]);
  return (v128_t)__r;
}
static __inline__ v128_t wasm_i32x4_trunc_sat_f64x2_zero(v128_t __a) {
  __f64x2 __x = (__f64x2)__a;
  __i32x4 __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = __i < 2 ? __builtin_wasm_trunc_saturate_s_i32_f64(__x[__i]) : 0;
  return (v128_t)__r;
}
static __inline__ v128_t wasm_u32x4_trunc_sat_f32x4(v128_t __a) {
  __f32x4 __x = (__f32x4)__a;
  __u32x4 __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __builtin_wasm_trunc_saturate_u_i32_f32(__x[__i]);
  return (v128_t)__r;
}
static __inline__ v128_t wasm_u32x4_trunc_sat_f64x2_zero(v128_t __a) {
  __f64x2 __x = (__f64x2)__a;
  __u32x4 __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = __i < 2 ? __builtin_wasm_trunc_saturate_u_i32_f64(__x[__i]) : 0;
  return (v128_t)__r;
}
static __inline__ v128_t wasm_f32x4_demote_f64x2_zero(v128_t __a) {
  __f64x2 __x = (__f64x2)__a;
  __f32x4 __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i < 2 ? (float)__x[__i] : 0;
  return (v128_t)__r;
}
static __inline__ v128_t wasm_f64x2_promote_low_f32x4(v128_t __a) {
  __f32x4 __x = (__f32x4)__a;
  __f64x2 __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __x[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_i8x16_narrow_i16x8(v128_t __a, v128_t __b) {
  __i16x8 __x = (__i16x8)__a;
  __i16x8 __y = (__i16x8)__b;
  int __s;
  __i8x16 __r;
  for (int __i = 0; __i < 16; __i++)
    __r[__i] = (__s = __i < 8 ? __x[__i] : __y[__i - 8]) < -128 ? -128 : __s > 127 ? 127 : __s;
  return (v128_t)__r;
}
static __inline__ v128_t wasm_u8x16_narrow_i16x8(v128_t __a, v128_t __b) {
  __i16x8 __x = (__i16x8)__a;
  __i16x8 __y = (__i16x8)__b;
  int __s;
  __u8x16 __r;
  for (int __i = 0; __i < 16; __i++)
    __r[__i] = (__s = __i < 8 ? __x[__i] : __y[__i - 8]) < 0 ? 0 : __s > 255 ? 255 : __s;
  return (v128_t)__r;
}
static __inline__ v128_t wasm_i16x8_narrow_i32x4(v128_t __a, v128_t __b) {
  __i32x4 __x = (__i32x4)__a;
  __i32x4 __y = (__i32x4)__b;
  int __s;
  __i16x8 __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (__s = __i < 4 ? __x[__i] : __y[__i - 4]) < -32768 ? -32768 : __s > 32767 ? 32767
        : __s;
  return (v128_t)__r;
}
static __inline__ v128_t wasm_u16x8_narrow_i32x4(v128_t __a, v128_t __b) {
  __i32x4 __x = (__i32x4)__a;
  __i32x4 __y = (__i32x4)__b;
  int __s;
  __u16x8 __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (__s = __i < 4 ? __x[__i] : __y[__i - 4]) < 0 ? 0 : __s > 65535 ? 65535 : __s;
  return (v128_t)__r;
}
static __inline__ v128_t wasm_i16x8_extend_low_i8x16(v128_t __a) {
  __i8x16 __x = (__i8x16)__a;
  __i16x8 __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __x[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_i16x8_extend_high_i8x16(v128_t __a) {
  __i8x16 __x = (__i8x16)__a;
  __i16x8 __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __x[__i + 8];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_u16x8_extend_low_u8x16(v128_t __a) {
  __u8x16 __x = (__u8x16)__a;
  __u16x8 __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __x[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_u16x8_extend_high_u8x16(v128_t __a) {
  __u8x16 __x = (__u8x16)__a;
  __u16x8 __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __x[__i + 8];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_i32x4_extend_low_i16x8(v128_t __a) {
  __i16x8 __x = (__i16x8)__a;
  __i32x4 __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __x[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_i32x4_extend_high_i16x8(v128_t __a) {
  __i16x8 __x = (__i16x8)__a;
  __i32x4 __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __x[__i + 4];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_u32x4_extend_low_u16x8(v128_t __a) {
  __u16x8 __x = (__u16x8)__a;
  __u32x4 __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __x[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_u32x4_extend_high_u16x8(v128_t __a) {
  __u16x8 __x = (__u16x8)__a;
  __u32x4 __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __x[__i + 4];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_i64x2_extend_low_i32x4(v128_t __a) {
  __i32x4 __x = (__i32x4)__a;
  __i64x2 __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __x[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_i64x2_extend_high_i32x4(v128_t __a) {
  __i32x4 __x = (__i32x4)__a;
  __i64x2 __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __x[__i + 2];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_u64x2_extend_low_u32x4(v128_t __a) {
  __u32x4 __x = (__u32x4)__a;
  __u64x2 __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __x[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_u64x2_extend_high_u32x4(v128_t __a) {
  __u32x4 __x = (__u32x4)__a;
  __u64x2 __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __x[__i + 2];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_i16x8_extmul_low_i8x16(v128_t __a, v128_t __b) {
  __i8x16 __x = (__i8x16)__a;
  __i8x16 __y = (__i8x16)__b;
  __i16x8 __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (short)__x[__i] * (short)__y[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_i16x8_extmul_high_i8x16(v128_t __a, v128_t __b) {
  __i8x16 __x = (__i8x16)__a;
  __i8x16 __y = (__i8x16)__b;
  __i16x8 __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (short)__x[__i + 8] * (short)__y[__i + 8];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_u16x8_extmul_low_u8x16(v128_t __a, v128_t __b) {
  __u8x16 __x = (__u8x16)__a;
  __u8x16 __y = (__u8x16)__b;
  __u16x8 __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (unsigned short)__x[__i] * (unsigned short)__y[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_u16x8_extmul_high_u8x16(v128_t __a, v128_t __b) {
  __u8x16 __x = (__u8x16)__a;
  __u8x16 __y = (__u8x16)__b;
  __u16x8 __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (unsigned short)__x[__i + 8] * (unsigned short)__y[__i + 8];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_i32x4_extmul_low_i16x8(v128_t __a, v128_t __b) {
  __i16x8 __x = (__i16x8)__a;
  __i16x8 __y = (__i16x8)__b;
  __i32x4 __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int)__x[__i] * (int)__y[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_i32x4_extmul_high_i16x8(v128_t __a, v128_t __b) {
  __i16x8 __x = (__i16x8)__a;
  __i16x8 __y = (__i16x8)__b;
  __i32x4 __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int)__x[__i + 4] * (int)__y[__i + 4];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_u32x4_extmul_low_u16x8(v128_t __a, v128_t __b) {
  __u16x8 __x = (__u16x8)__a;
  __u16x8 __y = (__u16x8)__b;
  __u32x4 __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (unsigned int)__x[__i] * (unsigned int)__y[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_u32x4_extmul_high_u16x8(v128_t __a, v128_t __b) {
  __u16x8 __x = (__u16x8)__a;
  __u16x8 __y = (__u16x8)__b;
  __u32x4 __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (unsigned int)__x[__i + 4] * (unsigned int)__y[__i + 4];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_i64x2_extmul_low_i32x4(v128_t __a, v128_t __b) {
  __i32x4 __x = (__i32x4)__a;
  __i32x4 __y = (__i32x4)__b;
  __i64x2 __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (long long)__x[__i] * (long long)__y[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_i64x2_extmul_high_i32x4(v128_t __a, v128_t __b) {
  __i32x4 __x = (__i32x4)__a;
  __i32x4 __y = (__i32x4)__b;
  __i64x2 __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (long long)__x[__i + 2] * (long long)__y[__i + 2];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_u64x2_extmul_low_u32x4(v128_t __a, v128_t __b) {
  __u32x4 __x = (__u32x4)__a;
  __u32x4 __y = (__u32x4)__b;
  __u64x2 __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (unsigned long long)__x[__i] * (unsigned long long)__y[__i];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_u64x2_extmul_high_u32x4(v128_t __a, v128_t __b) {
  __u32x4 __x = (__u32x4)__a;
  __u32x4 __y = (__u32x4)__b;
  __u64x2 __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (unsigned long long)__x[__i + 2] * (unsigned long long)__y[__i + 2];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_i16x8_extadd_pairwise_i8x16(v128_t __a) {
  __i8x16 __x = (__i8x16)__a;
  __i16x8 __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __x[2 * __i] + __x[2 * __i + 1];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_u16x8_extadd_pairwise_u8x16(v128_t __a) {
  __u8x16 __x = (__u8x16)__a;
  __u16x8 __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __x[2 * __i] + __x[2 * __i + 1];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_i32x4_extadd_pairwise_i16x8(v128_t __a) {
  __i16x8 __x = (__i16x8)__a;
  __i32x4 __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __x[2 * __i] + __x[2 * __i + 1];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_u32x4_extadd_pairwise_u16x8(v128_t __a) {
  __u16x8 __x = (__u16x8)__a;
  __u32x4 __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __x[2 * __i] + __x[2 * __i + 1];
  return (v128_t)__r;
}

/* Bitwise operations, the any test and the swizzle. */
static __inline__ v128_t wasm_v128_not(v128_t __a) {
  return ~__a;
}
static __inline__ v128_t wasm_v128_and(v128_t __a, v128_t __b) {
  return __a & __b;
}
static __inline__ v128_t wasm_v128_or(v128_t __a, v128_t __b) {
  return __a | __b;
}
static __inline__ v128_t wasm_v128_xor(v128_t __a, v128_t __b) {
  return __a ^ __b;
}
static __inline__ v128_t wasm_v128_andnot(v128_t __a, v128_t __b) {
  return __a & ~__b;
}
static __inline__ bool wasm_v128_any_true(v128_t __a) {
  __u64x2 __x = (__u64x2)__a;
  return (__x[0] | __x[1]) != 0;
}
static __inline__ v128_t wasm_v128_bitselect(v128_t __a, v128_t __b, v128_t __m) {
  return (__a & __m) | (__b & ~__m);
}
static __inline__ v128_t wasm_i8x16_swizzle(v128_t __a, v128_t __b) {
  __u8x16 __x = (__u8x16)__a;
  __u8x16 __y = (__u8x16)__b;
  __u8x16 __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __y[__i] < 16 ? __x[__y[__i]] : 0;
  return (v128_t)__r;
}

/* The relaxed-simd functions. Each gives one of the answers the proposal allows, and the same
 * one on every engine. */
static __inline__ v128_t wasm_i8x16_relaxed_swizzle(v128_t __a, v128_t __s) {
  __u8x16 __x = (__u8x16)__a;
  __u8x16 __y = (__u8x16)__s;
  __u8x16 __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __y[__i] < 16 ? __x[__y[__i]] : 0;
  return (v128_t)__r;
}
static __inline__ v128_t wasm_i32x4_relaxed_trunc_f32x4(v128_t __a) {
  return wasm_i32x4_trunc_sat_f32x4(__a);
}
static __inline__ v128_t wasm_i32x4_relaxed_trunc_f64x2_zero(v128_t __a) {
  return wasm_i32x4_trunc_sat_f64x2_zero(__a);
}
static __inline__ v128_t wasm_u32x4_relaxed_trunc_f32x4(v128_t __a) {
  return wasm_u32x4_trunc_sat_f32x4(__a);
}
static __inline__ v128_t wasm_u32x4_relaxed_trunc_f64x2_zero(v128_t __a) {
  return wasm_u32x4_trunc_sat_f64x2_zero(__a);
}
static __inline__ v128_t wasm_f32x4_relaxed_madd(v128_t __a, v128_t __b, v128_t __c) {
  return (v128_t)((__f32x4)__a * (__f32x4)__b + (__f32x4)__c);
}
static __inline__ v128_t wasm_f32x4_relaxed_nmadd(v128_t __a, v128_t __b, v128_t __c) {
  return (v128_t)(-((__f32x4)__a * (__f32x4)__b) + (__f32x4)__c);
}
static __inline__ v128_t wasm_f32x4_relaxed_min(v128_t __a, v128_t __b) {
  return wasm_f32x4_min(__a, __b);
}
static __inline__ v128_t wasm_f32x4_relaxed_max(v128_t __a, v128_t __b) {
  return wasm_f32x4_max(__a, __b);
}
static __inline__ v128_t wasm_f64x2_relaxed_madd(v128_t __a, v128_t __b, v128_t __c) {
  return (v128_t)((__f64x2)__a * (__f64x2)__b + (__f64x2)__c);
}
static __inline__ v128_t wasm_f64x2_relaxed_nmadd(v128_t __a, v128_t __b, v128_t __c) {
  return (v128_t)(-((__f64x2)__a * (__f64x2)__b) + (__f64x2)__c);
}
static __inline__ v128_t wasm_f64x2_relaxed_min(v128_t __a, v128_t __b) {
  return wasm_f64x2_min(__a, __b);
}
static __inline__ v128_t wasm_f64x2_relaxed_max(v128_t __a, v128_t __b) {
  return wasm_f64x2_max(__a, __b);
}
static __inline__ v128_t wasm_i8x16_relaxed_laneselect(v128_t __a, v128_t __b, v128_t __m) {
  return wasm_v128_bitselect(__a, __b, __m);
}
static __inline__ v128_t wasm_i16x8_relaxed_laneselect(v128_t __a, v128_t __b, v128_t __m) {
  return wasm_v128_bitselect(__a, __b, __m);
}
static __inline__ v128_t wasm_i32x4_relaxed_laneselect(v128_t __a, v128_t __b, v128_t __m) {
  return wasm_v128_bitselect(__a, __b, __m);
}
static __inline__ v128_t wasm_i64x2_relaxed_laneselect(v128_t __a, v128_t __b, v128_t __m) {
  return wasm_v128_bitselect(__a, __b, __m);
}
static __inline__ v128_t wasm_i16x8_relaxed_q15mulr(v128_t __a, v128_t __b) {
  return wasm_i16x8_q15mulr_sat(__a, __b);
}
static __inline__ v128_t wasm_i16x8_relaxed_dot_i8x16_i7x16(v128_t __a, v128_t __b) {
  __i8x16 __x = (__i8x16)__a;
  __i8x16 __y = (__i8x16)__b;
  __i16x8 __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = __x[2 * __i] * __y[2 * __i] + __x[2 * __i + 1] * __y[2 * __i + 1];
  return (v128_t)__r;
}
static __inline__ v128_t wasm_i32x4_relaxed_dot_i8x16_i7x16_add(v128_t __a, v128_t __b,
    v128_t __c) {
  __i8x16 __x = (__i8x16)__a;
  __i8x16 __y = (__i8x16)__b;
  __u32x4 __z = (__u32x4)__c;
  __u32x4 __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (unsigned)(__x[4 * __i] * __y[4 * __i] + __x[4 * __i + 1] * __y[4 * __i + 1]
        + __x[4 * __i + 2] * __y[4 * __i + 2] + __x[4 * __i + 3] * __y[4 * __i + 3]) + __z[__i];
  return (v128_t)__r;
}

/* The shuffles, which pick each lane of the answer from the lanes of both operands. A lane index
 * past the end of both picks from the start again, as `__builtin_shuffle` does. */
#define wasm_i8x16_shuffle(__a, __b, __c0, __c1, __c2, __c3, __c4, __c5, __c6, __c7, __c8, __c9, \
  __c10, __c11, __c12, __c13, __c14, __c15) \
  ((v128_t)__builtin_shuffle((__u8x16)(__a), (__u8x16)(__b), (__u8x16){__c0, __c1, __c2, __c3, \
    __c4, __c5, __c6, __c7, __c8, __c9, __c10, __c11, __c12, __c13, __c14, __c15}))
#define wasm_i16x8_shuffle(__a, __b, __c0, __c1, __c2, __c3, __c4, __c5, __c6, __c7) \
  ((v128_t)__builtin_shuffle((__u16x8)(__a), (__u16x8)(__b), (__u16x8){__c0, __c1, __c2, __c3, \
    __c4, __c5, __c6, __c7}))
#define wasm_i32x4_shuffle(__a, __b, __c0, __c1, __c2, __c3) \
  ((v128_t)__builtin_shuffle((__u32x4)(__a), (__u32x4)(__b), (__u32x4){__c0, __c1, __c2, __c3}))
#define wasm_i64x2_shuffle(__a, __b, __c0, __c1) \
  ((v128_t)__builtin_shuffle((__u64x2)(__a), (__u64x2)(__b), (__u64x2){__c0, __c1}))

/* The names of earlier versions of the proposal, which clang keeps as deprecated. */
static __inline__ __attribute__((__deprecated__("use wasm_v128_load8_splat instead")))
v128_t wasm_v8x16_load_splat(const void *__mem) {
  return wasm_v128_load8_splat(__mem);
}
static __inline__ __attribute__((__deprecated__("use wasm_v128_load16_splat instead")))
v128_t wasm_v16x8_load_splat(const void *__mem) {
  return wasm_v128_load16_splat(__mem);
}
static __inline__ __attribute__((__deprecated__("use wasm_v128_load32_splat instead")))
v128_t wasm_v32x4_load_splat(const void *__mem) {
  return wasm_v128_load32_splat(__mem);
}
static __inline__ __attribute__((__deprecated__("use wasm_v128_load64_splat instead")))
v128_t wasm_v64x2_load_splat(const void *__mem) {
  return wasm_v128_load64_splat(__mem);
}
static __inline__ __attribute__((__deprecated__("use wasm_i16x8_load8x8 instead")))
v128_t wasm_i16x8_load_8x8(const void *__mem) {
  return wasm_i16x8_load8x8(__mem);
}
static __inline__ __attribute__((__deprecated__("use wasm_u16x8_load8x8 instead")))
v128_t wasm_u16x8_load_8x8(const void *__mem) {
  return wasm_u16x8_load8x8(__mem);
}
static __inline__ __attribute__((__deprecated__("use wasm_i32x4_load16x4 instead")))
v128_t wasm_i32x4_load_16x4(const void *__mem) {
  return wasm_i32x4_load16x4(__mem);
}
static __inline__ __attribute__((__deprecated__("use wasm_u32x4_load16x4 instead")))
v128_t wasm_u32x4_load_16x4(const void *__mem) {
  return wasm_u32x4_load16x4(__mem);
}
static __inline__ __attribute__((__deprecated__("use wasm_i64x2_load32x2 instead")))
v128_t wasm_i64x2_load_32x2(const void *__mem) {
  return wasm_i64x2_load32x2(__mem);
}
static __inline__ __attribute__((__deprecated__("use wasm_u64x2_load32x2 instead")))
v128_t wasm_u64x2_load_32x2(const void *__mem) {
  return wasm_u64x2_load32x2(__mem);
}
static __inline__ __attribute__((__deprecated__("use wasm_i8x16_swizzle instead")))
v128_t wasm_v8x16_swizzle(v128_t __a, v128_t __b) {
  return wasm_i8x16_swizzle(__a, __b);
}
static __inline__ __attribute__((__deprecated__("use wasm_v128_any_true instead")))
bool wasm_i8x16_any_true(v128_t __a) {
  return wasm_v128_any_true(__a);
}
static __inline__ __attribute__((__deprecated__("use wasm_v128_any_true instead")))
bool wasm_i16x8_any_true(v128_t __a) {
  return wasm_v128_any_true(__a);
}
static __inline__ __attribute__((__deprecated__("use wasm_v128_any_true instead")))
bool wasm_i32x4_any_true(v128_t __a) {
  return wasm_v128_any_true(__a);
}
static __inline__ __attribute__((__deprecated__("use wasm_i8x16_add_sat instead")))
v128_t wasm_i8x16_add_saturate(v128_t __a, v128_t __b) {
  return wasm_i8x16_add_sat(__a, __b);
}
static __inline__ __attribute__((__deprecated__("use wasm_i8x16_sub_sat instead")))
v128_t wasm_i8x16_sub_saturate(v128_t __a, v128_t __b) {
  return wasm_i8x16_sub_sat(__a, __b);
}
static __inline__ __attribute__((__deprecated__("use wasm_u8x16_add_sat instead")))
v128_t wasm_u8x16_add_saturate(v128_t __a, v128_t __b) {
  return wasm_u8x16_add_sat(__a, __b);
}
static __inline__ __attribute__((__deprecated__("use wasm_u8x16_sub_sat instead")))
v128_t wasm_u8x16_sub_saturate(v128_t __a, v128_t __b) {
  return wasm_u8x16_sub_sat(__a, __b);
}
static __inline__ __attribute__((__deprecated__("use wasm_i16x8_add_sat instead")))
v128_t wasm_i16x8_add_saturate(v128_t __a, v128_t __b) {
  return wasm_i16x8_add_sat(__a, __b);
}
static __inline__ __attribute__((__deprecated__("use wasm_i16x8_sub_sat instead")))
v128_t wasm_i16x8_sub_saturate(v128_t __a, v128_t __b) {
  return wasm_i16x8_sub_sat(__a, __b);
}
static __inline__ __attribute__((__deprecated__("use wasm_u16x8_add_sat instead")))
v128_t wasm_u16x8_add_saturate(v128_t __a, v128_t __b) {
  return wasm_u16x8_add_sat(__a, __b);
}
static __inline__ __attribute__((__deprecated__("use wasm_u16x8_sub_sat instead")))
v128_t wasm_u16x8_sub_saturate(v128_t __a, v128_t __b) {
  return wasm_u16x8_sub_sat(__a, __b);
}
static __inline__ __attribute__((__deprecated__("use wasm_i16x8_extend_low_i8x16 instead")))
v128_t wasm_i16x8_widen_low_i8x16(v128_t __a) {
  return wasm_i16x8_extend_low_i8x16(__a);
}
static __inline__ __attribute__((__deprecated__("use wasm_i16x8_extend_high_i8x16 instead")))
v128_t wasm_i16x8_widen_high_i8x16(v128_t __a) {
  return wasm_i16x8_extend_high_i8x16(__a);
}
static __inline__ __attribute__((__deprecated__("use wasm_u16x8_extend_low_u8x16 instead")))
v128_t wasm_i16x8_widen_low_u8x16(v128_t __a) {
  return wasm_u16x8_extend_low_u8x16(__a);
}
static __inline__ __attribute__((__deprecated__("use wasm_u16x8_extend_high_u8x16 instead")))
v128_t wasm_i16x8_widen_high_u8x16(v128_t __a) {
  return wasm_u16x8_extend_high_u8x16(__a);
}
static __inline__ __attribute__((__deprecated__("use wasm_i32x4_extend_low_i16x8 instead")))
v128_t wasm_i32x4_widen_low_i16x8(v128_t __a) {
  return wasm_i32x4_extend_low_i16x8(__a);
}
static __inline__ __attribute__((__deprecated__("use wasm_i32x4_extend_high_i16x8 instead")))
v128_t wasm_i32x4_widen_high_i16x8(v128_t __a) {
  return wasm_i32x4_extend_high_i16x8(__a);
}
static __inline__ __attribute__((__deprecated__("use wasm_u32x4_extend_low_u16x8 instead")))
v128_t wasm_i32x4_widen_low_u16x8(v128_t __a) {
  return wasm_u32x4_extend_low_u16x8(__a);
}
static __inline__ __attribute__((__deprecated__("use wasm_u32x4_extend_high_u16x8 instead")))
v128_t wasm_i32x4_widen_high_u16x8(v128_t __a) {
  return wasm_u32x4_extend_high_u16x8(__a);
}
static __inline__ __attribute__((__deprecated__("use wasm_i32x4_trunc_sat_f32x4 instead")))
v128_t wasm_i32x4_trunc_saturate_f32x4(v128_t __a) {
  return wasm_i32x4_trunc_sat_f32x4(__a);
}
static __inline__ __attribute__((__deprecated__("use wasm_u32x4_trunc_sat_f32x4 instead")))
v128_t wasm_u32x4_trunc_saturate_f32x4(v128_t __a) {
  return wasm_u32x4_trunc_sat_f32x4(__a);
}
#define wasm_v8x16_shuffle wasm_i8x16_shuffle
#define wasm_v16x8_shuffle wasm_i16x8_shuffle
#define wasm_v32x4_shuffle wasm_i32x4_shuffle
#define wasm_v64x2_shuffle wasm_i64x2_shuffle

#endif
