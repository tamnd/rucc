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

#ifndef __RUCC_ARM_NEON_H
#define __RUCC_ARM_NEON_H

#if !defined(__aarch64__)
#error "arm_neon.h is for AArch64"
#endif

#include <stdint.h>

typedef float float32_t;
typedef double float64_t;
typedef uint8_t poly8_t;
typedef uint16_t poly16_t;
typedef uint64_t poly64_t;

typedef __Int8x8_t int8x8_t;
typedef __Int16x4_t int16x4_t;
typedef __Int32x2_t int32x2_t;
typedef __Int64x1_t int64x1_t;
typedef __Uint8x8_t uint8x8_t;
typedef __Uint16x4_t uint16x4_t;
typedef __Uint32x2_t uint32x2_t;
typedef __Uint64x1_t uint64x1_t;
typedef __Float32x2_t float32x2_t;
typedef __Float64x1_t float64x1_t;
typedef __Poly8x8_t poly8x8_t;
typedef __Poly16x4_t poly16x4_t;
typedef __Poly64x1_t poly64x1_t;
typedef __Int8x16_t int8x16_t;
typedef __Int16x8_t int16x8_t;
typedef __Int32x4_t int32x4_t;
typedef __Int64x2_t int64x2_t;
typedef __Uint8x16_t uint8x16_t;
typedef __Uint16x8_t uint16x8_t;
typedef __Uint32x4_t uint32x4_t;
typedef __Uint64x2_t uint64x2_t;
typedef __Float32x4_t float32x4_t;
typedef __Float64x2_t float64x2_t;
typedef __Poly8x16_t poly8x16_t;
typedef __Poly16x8_t poly16x8_t;
typedef __Poly64x2_t poly64x2_t;

typedef struct int8x8x2_t { int8x8_t val[2]; } int8x8x2_t;
typedef struct int8x8x3_t { int8x8_t val[3]; } int8x8x3_t;
typedef struct int8x8x4_t { int8x8_t val[4]; } int8x8x4_t;
typedef struct int16x4x2_t { int16x4_t val[2]; } int16x4x2_t;
typedef struct int16x4x3_t { int16x4_t val[3]; } int16x4x3_t;
typedef struct int16x4x4_t { int16x4_t val[4]; } int16x4x4_t;
typedef struct int32x2x2_t { int32x2_t val[2]; } int32x2x2_t;
typedef struct int32x2x3_t { int32x2_t val[3]; } int32x2x3_t;
typedef struct int32x2x4_t { int32x2_t val[4]; } int32x2x4_t;
typedef struct int64x1x2_t { int64x1_t val[2]; } int64x1x2_t;
typedef struct int64x1x3_t { int64x1_t val[3]; } int64x1x3_t;
typedef struct int64x1x4_t { int64x1_t val[4]; } int64x1x4_t;
typedef struct uint8x8x2_t { uint8x8_t val[2]; } uint8x8x2_t;
typedef struct uint8x8x3_t { uint8x8_t val[3]; } uint8x8x3_t;
typedef struct uint8x8x4_t { uint8x8_t val[4]; } uint8x8x4_t;
typedef struct uint16x4x2_t { uint16x4_t val[2]; } uint16x4x2_t;
typedef struct uint16x4x3_t { uint16x4_t val[3]; } uint16x4x3_t;
typedef struct uint16x4x4_t { uint16x4_t val[4]; } uint16x4x4_t;
typedef struct uint32x2x2_t { uint32x2_t val[2]; } uint32x2x2_t;
typedef struct uint32x2x3_t { uint32x2_t val[3]; } uint32x2x3_t;
typedef struct uint32x2x4_t { uint32x2_t val[4]; } uint32x2x4_t;
typedef struct uint64x1x2_t { uint64x1_t val[2]; } uint64x1x2_t;
typedef struct uint64x1x3_t { uint64x1_t val[3]; } uint64x1x3_t;
typedef struct uint64x1x4_t { uint64x1_t val[4]; } uint64x1x4_t;
typedef struct float32x2x2_t { float32x2_t val[2]; } float32x2x2_t;
typedef struct float32x2x3_t { float32x2_t val[3]; } float32x2x3_t;
typedef struct float32x2x4_t { float32x2_t val[4]; } float32x2x4_t;
typedef struct float64x1x2_t { float64x1_t val[2]; } float64x1x2_t;
typedef struct float64x1x3_t { float64x1_t val[3]; } float64x1x3_t;
typedef struct float64x1x4_t { float64x1_t val[4]; } float64x1x4_t;
typedef struct poly8x8x2_t { poly8x8_t val[2]; } poly8x8x2_t;
typedef struct poly8x8x3_t { poly8x8_t val[3]; } poly8x8x3_t;
typedef struct poly8x8x4_t { poly8x8_t val[4]; } poly8x8x4_t;
typedef struct poly16x4x2_t { poly16x4_t val[2]; } poly16x4x2_t;
typedef struct poly16x4x3_t { poly16x4_t val[3]; } poly16x4x3_t;
typedef struct poly16x4x4_t { poly16x4_t val[4]; } poly16x4x4_t;
typedef struct poly64x1x2_t { poly64x1_t val[2]; } poly64x1x2_t;
typedef struct poly64x1x3_t { poly64x1_t val[3]; } poly64x1x3_t;
typedef struct poly64x1x4_t { poly64x1_t val[4]; } poly64x1x4_t;
typedef struct int8x16x2_t { int8x16_t val[2]; } int8x16x2_t;
typedef struct int8x16x3_t { int8x16_t val[3]; } int8x16x3_t;
typedef struct int8x16x4_t { int8x16_t val[4]; } int8x16x4_t;
typedef struct int16x8x2_t { int16x8_t val[2]; } int16x8x2_t;
typedef struct int16x8x3_t { int16x8_t val[3]; } int16x8x3_t;
typedef struct int16x8x4_t { int16x8_t val[4]; } int16x8x4_t;
typedef struct int32x4x2_t { int32x4_t val[2]; } int32x4x2_t;
typedef struct int32x4x3_t { int32x4_t val[3]; } int32x4x3_t;
typedef struct int32x4x4_t { int32x4_t val[4]; } int32x4x4_t;
typedef struct int64x2x2_t { int64x2_t val[2]; } int64x2x2_t;
typedef struct int64x2x3_t { int64x2_t val[3]; } int64x2x3_t;
typedef struct int64x2x4_t { int64x2_t val[4]; } int64x2x4_t;
typedef struct uint8x16x2_t { uint8x16_t val[2]; } uint8x16x2_t;
typedef struct uint8x16x3_t { uint8x16_t val[3]; } uint8x16x3_t;
typedef struct uint8x16x4_t { uint8x16_t val[4]; } uint8x16x4_t;
typedef struct uint16x8x2_t { uint16x8_t val[2]; } uint16x8x2_t;
typedef struct uint16x8x3_t { uint16x8_t val[3]; } uint16x8x3_t;
typedef struct uint16x8x4_t { uint16x8_t val[4]; } uint16x8x4_t;
typedef struct uint32x4x2_t { uint32x4_t val[2]; } uint32x4x2_t;
typedef struct uint32x4x3_t { uint32x4_t val[3]; } uint32x4x3_t;
typedef struct uint32x4x4_t { uint32x4_t val[4]; } uint32x4x4_t;
typedef struct uint64x2x2_t { uint64x2_t val[2]; } uint64x2x2_t;
typedef struct uint64x2x3_t { uint64x2_t val[3]; } uint64x2x3_t;
typedef struct uint64x2x4_t { uint64x2_t val[4]; } uint64x2x4_t;
typedef struct float32x4x2_t { float32x4_t val[2]; } float32x4x2_t;
typedef struct float32x4x3_t { float32x4_t val[3]; } float32x4x3_t;
typedef struct float32x4x4_t { float32x4_t val[4]; } float32x4x4_t;
typedef struct float64x2x2_t { float64x2_t val[2]; } float64x2x2_t;
typedef struct float64x2x3_t { float64x2_t val[3]; } float64x2x3_t;
typedef struct float64x2x4_t { float64x2_t val[4]; } float64x2x4_t;
typedef struct poly8x16x2_t { poly8x16_t val[2]; } poly8x16x2_t;
typedef struct poly8x16x3_t { poly8x16_t val[3]; } poly8x16x3_t;
typedef struct poly8x16x4_t { poly8x16_t val[4]; } poly8x16x4_t;
typedef struct poly16x8x2_t { poly16x8_t val[2]; } poly16x8x2_t;
typedef struct poly16x8x3_t { poly16x8_t val[3]; } poly16x8x3_t;
typedef struct poly16x8x4_t { poly16x8_t val[4]; } poly16x8x4_t;
typedef struct poly64x2x2_t { poly64x2_t val[2]; } poly64x2x2_t;
typedef struct poly64x2x3_t { poly64x2_t val[3]; } poly64x2x3_t;
typedef struct poly64x2x4_t { poly64x2_t val[4]; } poly64x2x4_t;

/* Loads and stores. Through memcpy, since neither has to be aligned. */
static __inline__ int8x8_t vld1_s8(const int8_t *__p) {
  int8x8_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_s8(int8_t *__p, int8x8_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ int8x8_t vld1_dup_s8(const int8_t *__p) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = *__p;
  return __r;
}
static __inline__ int8x8_t vld1_lane_s8(const int8_t *__p, int8x8_t __v, const int __lane) {
  __v[__lane] = *__p;
  return __v;
}
static __inline__ void vst1_lane_s8(int8_t *__p, int8x8_t __v, const int __lane) {
  *__p = __v[__lane];
}
static __inline__ int8x8x2_t vld1_s8_x2(const int8_t *__p) {
  int8x8x2_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_s8_x2(int8_t *__p, int8x8x2_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ int8x8x2_t vld2_s8(const int8_t *__p) {
  int8x8x2_t __r;
  for (int __i = 0; __i < 8; __i++)
    for (int __j = 0; __j < 2; __j++) __r.val[__j][__i] = __p[__i * 2 + __j];
  return __r;
}
static __inline__ void vst2_s8(int8_t *__p, int8x8x2_t __v) {
  for (int __i = 0; __i < 8; __i++)
    for (int __j = 0; __j < 2; __j++) __p[__i * 2 + __j] = __v.val[__j][__i];
}
static __inline__ int8x8x3_t vld1_s8_x3(const int8_t *__p) {
  int8x8x3_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_s8_x3(int8_t *__p, int8x8x3_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ int8x8x3_t vld3_s8(const int8_t *__p) {
  int8x8x3_t __r;
  for (int __i = 0; __i < 8; __i++)
    for (int __j = 0; __j < 3; __j++) __r.val[__j][__i] = __p[__i * 3 + __j];
  return __r;
}
static __inline__ void vst3_s8(int8_t *__p, int8x8x3_t __v) {
  for (int __i = 0; __i < 8; __i++)
    for (int __j = 0; __j < 3; __j++) __p[__i * 3 + __j] = __v.val[__j][__i];
}
static __inline__ int8x8x4_t vld1_s8_x4(const int8_t *__p) {
  int8x8x4_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_s8_x4(int8_t *__p, int8x8x4_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ int8x8x4_t vld4_s8(const int8_t *__p) {
  int8x8x4_t __r;
  for (int __i = 0; __i < 8; __i++)
    for (int __j = 0; __j < 4; __j++) __r.val[__j][__i] = __p[__i * 4 + __j];
  return __r;
}
static __inline__ void vst4_s8(int8_t *__p, int8x8x4_t __v) {
  for (int __i = 0; __i < 8; __i++)
    for (int __j = 0; __j < 4; __j++) __p[__i * 4 + __j] = __v.val[__j][__i];
}
static __inline__ int16x4_t vld1_s16(const int16_t *__p) {
  int16x4_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_s16(int16_t *__p, int16x4_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ int16x4_t vld1_dup_s16(const int16_t *__p) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = *__p;
  return __r;
}
static __inline__ int16x4_t vld1_lane_s16(const int16_t *__p, int16x4_t __v, const int __lane) {
  __v[__lane] = *__p;
  return __v;
}
static __inline__ void vst1_lane_s16(int16_t *__p, int16x4_t __v, const int __lane) {
  *__p = __v[__lane];
}
static __inline__ int16x4x2_t vld1_s16_x2(const int16_t *__p) {
  int16x4x2_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_s16_x2(int16_t *__p, int16x4x2_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ int16x4x2_t vld2_s16(const int16_t *__p) {
  int16x4x2_t __r;
  for (int __i = 0; __i < 4; __i++)
    for (int __j = 0; __j < 2; __j++) __r.val[__j][__i] = __p[__i * 2 + __j];
  return __r;
}
static __inline__ void vst2_s16(int16_t *__p, int16x4x2_t __v) {
  for (int __i = 0; __i < 4; __i++)
    for (int __j = 0; __j < 2; __j++) __p[__i * 2 + __j] = __v.val[__j][__i];
}
static __inline__ int16x4x3_t vld1_s16_x3(const int16_t *__p) {
  int16x4x3_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_s16_x3(int16_t *__p, int16x4x3_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ int16x4x3_t vld3_s16(const int16_t *__p) {
  int16x4x3_t __r;
  for (int __i = 0; __i < 4; __i++)
    for (int __j = 0; __j < 3; __j++) __r.val[__j][__i] = __p[__i * 3 + __j];
  return __r;
}
static __inline__ void vst3_s16(int16_t *__p, int16x4x3_t __v) {
  for (int __i = 0; __i < 4; __i++)
    for (int __j = 0; __j < 3; __j++) __p[__i * 3 + __j] = __v.val[__j][__i];
}
static __inline__ int16x4x4_t vld1_s16_x4(const int16_t *__p) {
  int16x4x4_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_s16_x4(int16_t *__p, int16x4x4_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ int16x4x4_t vld4_s16(const int16_t *__p) {
  int16x4x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    for (int __j = 0; __j < 4; __j++) __r.val[__j][__i] = __p[__i * 4 + __j];
  return __r;
}
static __inline__ void vst4_s16(int16_t *__p, int16x4x4_t __v) {
  for (int __i = 0; __i < 4; __i++)
    for (int __j = 0; __j < 4; __j++) __p[__i * 4 + __j] = __v.val[__j][__i];
}
static __inline__ int32x2_t vld1_s32(const int32_t *__p) {
  int32x2_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_s32(int32_t *__p, int32x2_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ int32x2_t vld1_dup_s32(const int32_t *__p) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = *__p;
  return __r;
}
static __inline__ int32x2_t vld1_lane_s32(const int32_t *__p, int32x2_t __v, const int __lane) {
  __v[__lane] = *__p;
  return __v;
}
static __inline__ void vst1_lane_s32(int32_t *__p, int32x2_t __v, const int __lane) {
  *__p = __v[__lane];
}
static __inline__ int32x2x2_t vld1_s32_x2(const int32_t *__p) {
  int32x2x2_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_s32_x2(int32_t *__p, int32x2x2_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ int32x2x2_t vld2_s32(const int32_t *__p) {
  int32x2x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 2; __j++) __r.val[__j][__i] = __p[__i * 2 + __j];
  return __r;
}
static __inline__ void vst2_s32(int32_t *__p, int32x2x2_t __v) {
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 2; __j++) __p[__i * 2 + __j] = __v.val[__j][__i];
}
static __inline__ int32x2x3_t vld1_s32_x3(const int32_t *__p) {
  int32x2x3_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_s32_x3(int32_t *__p, int32x2x3_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ int32x2x3_t vld3_s32(const int32_t *__p) {
  int32x2x3_t __r;
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 3; __j++) __r.val[__j][__i] = __p[__i * 3 + __j];
  return __r;
}
static __inline__ void vst3_s32(int32_t *__p, int32x2x3_t __v) {
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 3; __j++) __p[__i * 3 + __j] = __v.val[__j][__i];
}
static __inline__ int32x2x4_t vld1_s32_x4(const int32_t *__p) {
  int32x2x4_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_s32_x4(int32_t *__p, int32x2x4_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ int32x2x4_t vld4_s32(const int32_t *__p) {
  int32x2x4_t __r;
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 4; __j++) __r.val[__j][__i] = __p[__i * 4 + __j];
  return __r;
}
static __inline__ void vst4_s32(int32_t *__p, int32x2x4_t __v) {
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 4; __j++) __p[__i * 4 + __j] = __v.val[__j][__i];
}
static __inline__ int64x1_t vld1_s64(const int64_t *__p) {
  int64x1_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_s64(int64_t *__p, int64x1_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ int64x1_t vld1_dup_s64(const int64_t *__p) {
  int64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = *__p;
  return __r;
}
static __inline__ int64x1_t vld1_lane_s64(const int64_t *__p, int64x1_t __v, const int __lane) {
  __v[__lane] = *__p;
  return __v;
}
static __inline__ void vst1_lane_s64(int64_t *__p, int64x1_t __v, const int __lane) {
  *__p = __v[__lane];
}
static __inline__ int64x1x2_t vld1_s64_x2(const int64_t *__p) {
  int64x1x2_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_s64_x2(int64_t *__p, int64x1x2_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ int64x1x2_t vld2_s64(const int64_t *__p) {
  int64x1x2_t __r;
  for (int __i = 0; __i < 1; __i++)
    for (int __j = 0; __j < 2; __j++) __r.val[__j][__i] = __p[__i * 2 + __j];
  return __r;
}
static __inline__ void vst2_s64(int64_t *__p, int64x1x2_t __v) {
  for (int __i = 0; __i < 1; __i++)
    for (int __j = 0; __j < 2; __j++) __p[__i * 2 + __j] = __v.val[__j][__i];
}
static __inline__ int64x1x3_t vld1_s64_x3(const int64_t *__p) {
  int64x1x3_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_s64_x3(int64_t *__p, int64x1x3_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ int64x1x3_t vld3_s64(const int64_t *__p) {
  int64x1x3_t __r;
  for (int __i = 0; __i < 1; __i++)
    for (int __j = 0; __j < 3; __j++) __r.val[__j][__i] = __p[__i * 3 + __j];
  return __r;
}
static __inline__ void vst3_s64(int64_t *__p, int64x1x3_t __v) {
  for (int __i = 0; __i < 1; __i++)
    for (int __j = 0; __j < 3; __j++) __p[__i * 3 + __j] = __v.val[__j][__i];
}
static __inline__ int64x1x4_t vld1_s64_x4(const int64_t *__p) {
  int64x1x4_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_s64_x4(int64_t *__p, int64x1x4_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ int64x1x4_t vld4_s64(const int64_t *__p) {
  int64x1x4_t __r;
  for (int __i = 0; __i < 1; __i++)
    for (int __j = 0; __j < 4; __j++) __r.val[__j][__i] = __p[__i * 4 + __j];
  return __r;
}
static __inline__ void vst4_s64(int64_t *__p, int64x1x4_t __v) {
  for (int __i = 0; __i < 1; __i++)
    for (int __j = 0; __j < 4; __j++) __p[__i * 4 + __j] = __v.val[__j][__i];
}
static __inline__ uint8x8_t vld1_u8(const uint8_t *__p) {
  uint8x8_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_u8(uint8_t *__p, uint8x8_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ uint8x8_t vld1_dup_u8(const uint8_t *__p) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = *__p;
  return __r;
}
static __inline__ uint8x8_t vld1_lane_u8(const uint8_t *__p, uint8x8_t __v, const int __lane) {
  __v[__lane] = *__p;
  return __v;
}
static __inline__ void vst1_lane_u8(uint8_t *__p, uint8x8_t __v, const int __lane) {
  *__p = __v[__lane];
}
static __inline__ uint8x8x2_t vld1_u8_x2(const uint8_t *__p) {
  uint8x8x2_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_u8_x2(uint8_t *__p, uint8x8x2_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ uint8x8x2_t vld2_u8(const uint8_t *__p) {
  uint8x8x2_t __r;
  for (int __i = 0; __i < 8; __i++)
    for (int __j = 0; __j < 2; __j++) __r.val[__j][__i] = __p[__i * 2 + __j];
  return __r;
}
static __inline__ void vst2_u8(uint8_t *__p, uint8x8x2_t __v) {
  for (int __i = 0; __i < 8; __i++)
    for (int __j = 0; __j < 2; __j++) __p[__i * 2 + __j] = __v.val[__j][__i];
}
static __inline__ uint8x8x3_t vld1_u8_x3(const uint8_t *__p) {
  uint8x8x3_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_u8_x3(uint8_t *__p, uint8x8x3_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ uint8x8x3_t vld3_u8(const uint8_t *__p) {
  uint8x8x3_t __r;
  for (int __i = 0; __i < 8; __i++)
    for (int __j = 0; __j < 3; __j++) __r.val[__j][__i] = __p[__i * 3 + __j];
  return __r;
}
static __inline__ void vst3_u8(uint8_t *__p, uint8x8x3_t __v) {
  for (int __i = 0; __i < 8; __i++)
    for (int __j = 0; __j < 3; __j++) __p[__i * 3 + __j] = __v.val[__j][__i];
}
static __inline__ uint8x8x4_t vld1_u8_x4(const uint8_t *__p) {
  uint8x8x4_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_u8_x4(uint8_t *__p, uint8x8x4_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ uint8x8x4_t vld4_u8(const uint8_t *__p) {
  uint8x8x4_t __r;
  for (int __i = 0; __i < 8; __i++)
    for (int __j = 0; __j < 4; __j++) __r.val[__j][__i] = __p[__i * 4 + __j];
  return __r;
}
static __inline__ void vst4_u8(uint8_t *__p, uint8x8x4_t __v) {
  for (int __i = 0; __i < 8; __i++)
    for (int __j = 0; __j < 4; __j++) __p[__i * 4 + __j] = __v.val[__j][__i];
}
static __inline__ uint16x4_t vld1_u16(const uint16_t *__p) {
  uint16x4_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_u16(uint16_t *__p, uint16x4_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ uint16x4_t vld1_dup_u16(const uint16_t *__p) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = *__p;
  return __r;
}
static __inline__ uint16x4_t vld1_lane_u16(const uint16_t *__p, uint16x4_t __v, const int __lane) {
  __v[__lane] = *__p;
  return __v;
}
static __inline__ void vst1_lane_u16(uint16_t *__p, uint16x4_t __v, const int __lane) {
  *__p = __v[__lane];
}
static __inline__ uint16x4x2_t vld1_u16_x2(const uint16_t *__p) {
  uint16x4x2_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_u16_x2(uint16_t *__p, uint16x4x2_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ uint16x4x2_t vld2_u16(const uint16_t *__p) {
  uint16x4x2_t __r;
  for (int __i = 0; __i < 4; __i++)
    for (int __j = 0; __j < 2; __j++) __r.val[__j][__i] = __p[__i * 2 + __j];
  return __r;
}
static __inline__ void vst2_u16(uint16_t *__p, uint16x4x2_t __v) {
  for (int __i = 0; __i < 4; __i++)
    for (int __j = 0; __j < 2; __j++) __p[__i * 2 + __j] = __v.val[__j][__i];
}
static __inline__ uint16x4x3_t vld1_u16_x3(const uint16_t *__p) {
  uint16x4x3_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_u16_x3(uint16_t *__p, uint16x4x3_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ uint16x4x3_t vld3_u16(const uint16_t *__p) {
  uint16x4x3_t __r;
  for (int __i = 0; __i < 4; __i++)
    for (int __j = 0; __j < 3; __j++) __r.val[__j][__i] = __p[__i * 3 + __j];
  return __r;
}
static __inline__ void vst3_u16(uint16_t *__p, uint16x4x3_t __v) {
  for (int __i = 0; __i < 4; __i++)
    for (int __j = 0; __j < 3; __j++) __p[__i * 3 + __j] = __v.val[__j][__i];
}
static __inline__ uint16x4x4_t vld1_u16_x4(const uint16_t *__p) {
  uint16x4x4_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_u16_x4(uint16_t *__p, uint16x4x4_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ uint16x4x4_t vld4_u16(const uint16_t *__p) {
  uint16x4x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    for (int __j = 0; __j < 4; __j++) __r.val[__j][__i] = __p[__i * 4 + __j];
  return __r;
}
static __inline__ void vst4_u16(uint16_t *__p, uint16x4x4_t __v) {
  for (int __i = 0; __i < 4; __i++)
    for (int __j = 0; __j < 4; __j++) __p[__i * 4 + __j] = __v.val[__j][__i];
}
static __inline__ uint32x2_t vld1_u32(const uint32_t *__p) {
  uint32x2_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_u32(uint32_t *__p, uint32x2_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ uint32x2_t vld1_dup_u32(const uint32_t *__p) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = *__p;
  return __r;
}
static __inline__ uint32x2_t vld1_lane_u32(const uint32_t *__p, uint32x2_t __v, const int __lane) {
  __v[__lane] = *__p;
  return __v;
}
static __inline__ void vst1_lane_u32(uint32_t *__p, uint32x2_t __v, const int __lane) {
  *__p = __v[__lane];
}
static __inline__ uint32x2x2_t vld1_u32_x2(const uint32_t *__p) {
  uint32x2x2_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_u32_x2(uint32_t *__p, uint32x2x2_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ uint32x2x2_t vld2_u32(const uint32_t *__p) {
  uint32x2x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 2; __j++) __r.val[__j][__i] = __p[__i * 2 + __j];
  return __r;
}
static __inline__ void vst2_u32(uint32_t *__p, uint32x2x2_t __v) {
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 2; __j++) __p[__i * 2 + __j] = __v.val[__j][__i];
}
static __inline__ uint32x2x3_t vld1_u32_x3(const uint32_t *__p) {
  uint32x2x3_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_u32_x3(uint32_t *__p, uint32x2x3_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ uint32x2x3_t vld3_u32(const uint32_t *__p) {
  uint32x2x3_t __r;
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 3; __j++) __r.val[__j][__i] = __p[__i * 3 + __j];
  return __r;
}
static __inline__ void vst3_u32(uint32_t *__p, uint32x2x3_t __v) {
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 3; __j++) __p[__i * 3 + __j] = __v.val[__j][__i];
}
static __inline__ uint32x2x4_t vld1_u32_x4(const uint32_t *__p) {
  uint32x2x4_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_u32_x4(uint32_t *__p, uint32x2x4_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ uint32x2x4_t vld4_u32(const uint32_t *__p) {
  uint32x2x4_t __r;
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 4; __j++) __r.val[__j][__i] = __p[__i * 4 + __j];
  return __r;
}
static __inline__ void vst4_u32(uint32_t *__p, uint32x2x4_t __v) {
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 4; __j++) __p[__i * 4 + __j] = __v.val[__j][__i];
}
static __inline__ uint64x1_t vld1_u64(const uint64_t *__p) {
  uint64x1_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_u64(uint64_t *__p, uint64x1_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ uint64x1_t vld1_dup_u64(const uint64_t *__p) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = *__p;
  return __r;
}
static __inline__ uint64x1_t vld1_lane_u64(const uint64_t *__p, uint64x1_t __v, const int __lane) {
  __v[__lane] = *__p;
  return __v;
}
static __inline__ void vst1_lane_u64(uint64_t *__p, uint64x1_t __v, const int __lane) {
  *__p = __v[__lane];
}
static __inline__ uint64x1x2_t vld1_u64_x2(const uint64_t *__p) {
  uint64x1x2_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_u64_x2(uint64_t *__p, uint64x1x2_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ uint64x1x2_t vld2_u64(const uint64_t *__p) {
  uint64x1x2_t __r;
  for (int __i = 0; __i < 1; __i++)
    for (int __j = 0; __j < 2; __j++) __r.val[__j][__i] = __p[__i * 2 + __j];
  return __r;
}
static __inline__ void vst2_u64(uint64_t *__p, uint64x1x2_t __v) {
  for (int __i = 0; __i < 1; __i++)
    for (int __j = 0; __j < 2; __j++) __p[__i * 2 + __j] = __v.val[__j][__i];
}
static __inline__ uint64x1x3_t vld1_u64_x3(const uint64_t *__p) {
  uint64x1x3_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_u64_x3(uint64_t *__p, uint64x1x3_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ uint64x1x3_t vld3_u64(const uint64_t *__p) {
  uint64x1x3_t __r;
  for (int __i = 0; __i < 1; __i++)
    for (int __j = 0; __j < 3; __j++) __r.val[__j][__i] = __p[__i * 3 + __j];
  return __r;
}
static __inline__ void vst3_u64(uint64_t *__p, uint64x1x3_t __v) {
  for (int __i = 0; __i < 1; __i++)
    for (int __j = 0; __j < 3; __j++) __p[__i * 3 + __j] = __v.val[__j][__i];
}
static __inline__ uint64x1x4_t vld1_u64_x4(const uint64_t *__p) {
  uint64x1x4_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_u64_x4(uint64_t *__p, uint64x1x4_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ uint64x1x4_t vld4_u64(const uint64_t *__p) {
  uint64x1x4_t __r;
  for (int __i = 0; __i < 1; __i++)
    for (int __j = 0; __j < 4; __j++) __r.val[__j][__i] = __p[__i * 4 + __j];
  return __r;
}
static __inline__ void vst4_u64(uint64_t *__p, uint64x1x4_t __v) {
  for (int __i = 0; __i < 1; __i++)
    for (int __j = 0; __j < 4; __j++) __p[__i * 4 + __j] = __v.val[__j][__i];
}
static __inline__ float32x2_t vld1_f32(const float32_t *__p) {
  float32x2_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_f32(float32_t *__p, float32x2_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ float32x2_t vld1_dup_f32(const float32_t *__p) {
  float32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = *__p;
  return __r;
}
static __inline__ float32x2_t vld1_lane_f32(const float32_t *__p, float32x2_t __v,
    const int __lane) {
  __v[__lane] = *__p;
  return __v;
}
static __inline__ void vst1_lane_f32(float32_t *__p, float32x2_t __v, const int __lane) {
  *__p = __v[__lane];
}
static __inline__ float32x2x2_t vld1_f32_x2(const float32_t *__p) {
  float32x2x2_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_f32_x2(float32_t *__p, float32x2x2_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ float32x2x2_t vld2_f32(const float32_t *__p) {
  float32x2x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 2; __j++) __r.val[__j][__i] = __p[__i * 2 + __j];
  return __r;
}
static __inline__ void vst2_f32(float32_t *__p, float32x2x2_t __v) {
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 2; __j++) __p[__i * 2 + __j] = __v.val[__j][__i];
}
static __inline__ float32x2x3_t vld1_f32_x3(const float32_t *__p) {
  float32x2x3_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_f32_x3(float32_t *__p, float32x2x3_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ float32x2x3_t vld3_f32(const float32_t *__p) {
  float32x2x3_t __r;
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 3; __j++) __r.val[__j][__i] = __p[__i * 3 + __j];
  return __r;
}
static __inline__ void vst3_f32(float32_t *__p, float32x2x3_t __v) {
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 3; __j++) __p[__i * 3 + __j] = __v.val[__j][__i];
}
static __inline__ float32x2x4_t vld1_f32_x4(const float32_t *__p) {
  float32x2x4_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_f32_x4(float32_t *__p, float32x2x4_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ float32x2x4_t vld4_f32(const float32_t *__p) {
  float32x2x4_t __r;
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 4; __j++) __r.val[__j][__i] = __p[__i * 4 + __j];
  return __r;
}
static __inline__ void vst4_f32(float32_t *__p, float32x2x4_t __v) {
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 4; __j++) __p[__i * 4 + __j] = __v.val[__j][__i];
}
static __inline__ float64x1_t vld1_f64(const float64_t *__p) {
  float64x1_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_f64(float64_t *__p, float64x1_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ float64x1_t vld1_dup_f64(const float64_t *__p) {
  float64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = *__p;
  return __r;
}
static __inline__ float64x1_t vld1_lane_f64(const float64_t *__p, float64x1_t __v,
    const int __lane) {
  __v[__lane] = *__p;
  return __v;
}
static __inline__ void vst1_lane_f64(float64_t *__p, float64x1_t __v, const int __lane) {
  *__p = __v[__lane];
}
static __inline__ float64x1x2_t vld1_f64_x2(const float64_t *__p) {
  float64x1x2_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_f64_x2(float64_t *__p, float64x1x2_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ float64x1x2_t vld2_f64(const float64_t *__p) {
  float64x1x2_t __r;
  for (int __i = 0; __i < 1; __i++)
    for (int __j = 0; __j < 2; __j++) __r.val[__j][__i] = __p[__i * 2 + __j];
  return __r;
}
static __inline__ void vst2_f64(float64_t *__p, float64x1x2_t __v) {
  for (int __i = 0; __i < 1; __i++)
    for (int __j = 0; __j < 2; __j++) __p[__i * 2 + __j] = __v.val[__j][__i];
}
static __inline__ float64x1x3_t vld1_f64_x3(const float64_t *__p) {
  float64x1x3_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_f64_x3(float64_t *__p, float64x1x3_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ float64x1x3_t vld3_f64(const float64_t *__p) {
  float64x1x3_t __r;
  for (int __i = 0; __i < 1; __i++)
    for (int __j = 0; __j < 3; __j++) __r.val[__j][__i] = __p[__i * 3 + __j];
  return __r;
}
static __inline__ void vst3_f64(float64_t *__p, float64x1x3_t __v) {
  for (int __i = 0; __i < 1; __i++)
    for (int __j = 0; __j < 3; __j++) __p[__i * 3 + __j] = __v.val[__j][__i];
}
static __inline__ float64x1x4_t vld1_f64_x4(const float64_t *__p) {
  float64x1x4_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_f64_x4(float64_t *__p, float64x1x4_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ float64x1x4_t vld4_f64(const float64_t *__p) {
  float64x1x4_t __r;
  for (int __i = 0; __i < 1; __i++)
    for (int __j = 0; __j < 4; __j++) __r.val[__j][__i] = __p[__i * 4 + __j];
  return __r;
}
static __inline__ void vst4_f64(float64_t *__p, float64x1x4_t __v) {
  for (int __i = 0; __i < 1; __i++)
    for (int __j = 0; __j < 4; __j++) __p[__i * 4 + __j] = __v.val[__j][__i];
}
static __inline__ poly8x8_t vld1_p8(const poly8_t *__p) {
  poly8x8_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_p8(poly8_t *__p, poly8x8_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ poly8x8_t vld1_dup_p8(const poly8_t *__p) {
  poly8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = *__p;
  return __r;
}
static __inline__ poly8x8_t vld1_lane_p8(const poly8_t *__p, poly8x8_t __v, const int __lane) {
  __v[__lane] = *__p;
  return __v;
}
static __inline__ void vst1_lane_p8(poly8_t *__p, poly8x8_t __v, const int __lane) {
  *__p = __v[__lane];
}
static __inline__ poly8x8x2_t vld1_p8_x2(const poly8_t *__p) {
  poly8x8x2_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_p8_x2(poly8_t *__p, poly8x8x2_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ poly8x8x2_t vld2_p8(const poly8_t *__p) {
  poly8x8x2_t __r;
  for (int __i = 0; __i < 8; __i++)
    for (int __j = 0; __j < 2; __j++) __r.val[__j][__i] = __p[__i * 2 + __j];
  return __r;
}
static __inline__ void vst2_p8(poly8_t *__p, poly8x8x2_t __v) {
  for (int __i = 0; __i < 8; __i++)
    for (int __j = 0; __j < 2; __j++) __p[__i * 2 + __j] = __v.val[__j][__i];
}
static __inline__ poly8x8x3_t vld1_p8_x3(const poly8_t *__p) {
  poly8x8x3_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_p8_x3(poly8_t *__p, poly8x8x3_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ poly8x8x3_t vld3_p8(const poly8_t *__p) {
  poly8x8x3_t __r;
  for (int __i = 0; __i < 8; __i++)
    for (int __j = 0; __j < 3; __j++) __r.val[__j][__i] = __p[__i * 3 + __j];
  return __r;
}
static __inline__ void vst3_p8(poly8_t *__p, poly8x8x3_t __v) {
  for (int __i = 0; __i < 8; __i++)
    for (int __j = 0; __j < 3; __j++) __p[__i * 3 + __j] = __v.val[__j][__i];
}
static __inline__ poly8x8x4_t vld1_p8_x4(const poly8_t *__p) {
  poly8x8x4_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_p8_x4(poly8_t *__p, poly8x8x4_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ poly8x8x4_t vld4_p8(const poly8_t *__p) {
  poly8x8x4_t __r;
  for (int __i = 0; __i < 8; __i++)
    for (int __j = 0; __j < 4; __j++) __r.val[__j][__i] = __p[__i * 4 + __j];
  return __r;
}
static __inline__ void vst4_p8(poly8_t *__p, poly8x8x4_t __v) {
  for (int __i = 0; __i < 8; __i++)
    for (int __j = 0; __j < 4; __j++) __p[__i * 4 + __j] = __v.val[__j][__i];
}
static __inline__ poly16x4_t vld1_p16(const poly16_t *__p) {
  poly16x4_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_p16(poly16_t *__p, poly16x4_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ poly16x4_t vld1_dup_p16(const poly16_t *__p) {
  poly16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = *__p;
  return __r;
}
static __inline__ poly16x4_t vld1_lane_p16(const poly16_t *__p, poly16x4_t __v, const int __lane) {
  __v[__lane] = *__p;
  return __v;
}
static __inline__ void vst1_lane_p16(poly16_t *__p, poly16x4_t __v, const int __lane) {
  *__p = __v[__lane];
}
static __inline__ poly16x4x2_t vld1_p16_x2(const poly16_t *__p) {
  poly16x4x2_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_p16_x2(poly16_t *__p, poly16x4x2_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ poly16x4x2_t vld2_p16(const poly16_t *__p) {
  poly16x4x2_t __r;
  for (int __i = 0; __i < 4; __i++)
    for (int __j = 0; __j < 2; __j++) __r.val[__j][__i] = __p[__i * 2 + __j];
  return __r;
}
static __inline__ void vst2_p16(poly16_t *__p, poly16x4x2_t __v) {
  for (int __i = 0; __i < 4; __i++)
    for (int __j = 0; __j < 2; __j++) __p[__i * 2 + __j] = __v.val[__j][__i];
}
static __inline__ poly16x4x3_t vld1_p16_x3(const poly16_t *__p) {
  poly16x4x3_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_p16_x3(poly16_t *__p, poly16x4x3_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ poly16x4x3_t vld3_p16(const poly16_t *__p) {
  poly16x4x3_t __r;
  for (int __i = 0; __i < 4; __i++)
    for (int __j = 0; __j < 3; __j++) __r.val[__j][__i] = __p[__i * 3 + __j];
  return __r;
}
static __inline__ void vst3_p16(poly16_t *__p, poly16x4x3_t __v) {
  for (int __i = 0; __i < 4; __i++)
    for (int __j = 0; __j < 3; __j++) __p[__i * 3 + __j] = __v.val[__j][__i];
}
static __inline__ poly16x4x4_t vld1_p16_x4(const poly16_t *__p) {
  poly16x4x4_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_p16_x4(poly16_t *__p, poly16x4x4_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ poly16x4x4_t vld4_p16(const poly16_t *__p) {
  poly16x4x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    for (int __j = 0; __j < 4; __j++) __r.val[__j][__i] = __p[__i * 4 + __j];
  return __r;
}
static __inline__ void vst4_p16(poly16_t *__p, poly16x4x4_t __v) {
  for (int __i = 0; __i < 4; __i++)
    for (int __j = 0; __j < 4; __j++) __p[__i * 4 + __j] = __v.val[__j][__i];
}
static __inline__ poly64x1_t vld1_p64(const poly64_t *__p) {
  poly64x1_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_p64(poly64_t *__p, poly64x1_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ poly64x1_t vld1_dup_p64(const poly64_t *__p) {
  poly64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = *__p;
  return __r;
}
static __inline__ poly64x1_t vld1_lane_p64(const poly64_t *__p, poly64x1_t __v, const int __lane) {
  __v[__lane] = *__p;
  return __v;
}
static __inline__ void vst1_lane_p64(poly64_t *__p, poly64x1_t __v, const int __lane) {
  *__p = __v[__lane];
}
static __inline__ poly64x1x2_t vld1_p64_x2(const poly64_t *__p) {
  poly64x1x2_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_p64_x2(poly64_t *__p, poly64x1x2_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ poly64x1x2_t vld2_p64(const poly64_t *__p) {
  poly64x1x2_t __r;
  for (int __i = 0; __i < 1; __i++)
    for (int __j = 0; __j < 2; __j++) __r.val[__j][__i] = __p[__i * 2 + __j];
  return __r;
}
static __inline__ void vst2_p64(poly64_t *__p, poly64x1x2_t __v) {
  for (int __i = 0; __i < 1; __i++)
    for (int __j = 0; __j < 2; __j++) __p[__i * 2 + __j] = __v.val[__j][__i];
}
static __inline__ poly64x1x3_t vld1_p64_x3(const poly64_t *__p) {
  poly64x1x3_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_p64_x3(poly64_t *__p, poly64x1x3_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ poly64x1x3_t vld3_p64(const poly64_t *__p) {
  poly64x1x3_t __r;
  for (int __i = 0; __i < 1; __i++)
    for (int __j = 0; __j < 3; __j++) __r.val[__j][__i] = __p[__i * 3 + __j];
  return __r;
}
static __inline__ void vst3_p64(poly64_t *__p, poly64x1x3_t __v) {
  for (int __i = 0; __i < 1; __i++)
    for (int __j = 0; __j < 3; __j++) __p[__i * 3 + __j] = __v.val[__j][__i];
}
static __inline__ poly64x1x4_t vld1_p64_x4(const poly64_t *__p) {
  poly64x1x4_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1_p64_x4(poly64_t *__p, poly64x1x4_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ poly64x1x4_t vld4_p64(const poly64_t *__p) {
  poly64x1x4_t __r;
  for (int __i = 0; __i < 1; __i++)
    for (int __j = 0; __j < 4; __j++) __r.val[__j][__i] = __p[__i * 4 + __j];
  return __r;
}
static __inline__ void vst4_p64(poly64_t *__p, poly64x1x4_t __v) {
  for (int __i = 0; __i < 1; __i++)
    for (int __j = 0; __j < 4; __j++) __p[__i * 4 + __j] = __v.val[__j][__i];
}
static __inline__ int8x16_t vld1q_s8(const int8_t *__p) {
  int8x16_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_s8(int8_t *__p, int8x16_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ int8x16_t vld1q_dup_s8(const int8_t *__p) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = *__p;
  return __r;
}
static __inline__ int8x16_t vld1q_lane_s8(const int8_t *__p, int8x16_t __v, const int __lane) {
  __v[__lane] = *__p;
  return __v;
}
static __inline__ void vst1q_lane_s8(int8_t *__p, int8x16_t __v, const int __lane) {
  *__p = __v[__lane];
}
static __inline__ int8x16x2_t vld1q_s8_x2(const int8_t *__p) {
  int8x16x2_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_s8_x2(int8_t *__p, int8x16x2_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ int8x16x2_t vld2q_s8(const int8_t *__p) {
  int8x16x2_t __r;
  for (int __i = 0; __i < 16; __i++)
    for (int __j = 0; __j < 2; __j++) __r.val[__j][__i] = __p[__i * 2 + __j];
  return __r;
}
static __inline__ void vst2q_s8(int8_t *__p, int8x16x2_t __v) {
  for (int __i = 0; __i < 16; __i++)
    for (int __j = 0; __j < 2; __j++) __p[__i * 2 + __j] = __v.val[__j][__i];
}
static __inline__ int8x16x3_t vld1q_s8_x3(const int8_t *__p) {
  int8x16x3_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_s8_x3(int8_t *__p, int8x16x3_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ int8x16x3_t vld3q_s8(const int8_t *__p) {
  int8x16x3_t __r;
  for (int __i = 0; __i < 16; __i++)
    for (int __j = 0; __j < 3; __j++) __r.val[__j][__i] = __p[__i * 3 + __j];
  return __r;
}
static __inline__ void vst3q_s8(int8_t *__p, int8x16x3_t __v) {
  for (int __i = 0; __i < 16; __i++)
    for (int __j = 0; __j < 3; __j++) __p[__i * 3 + __j] = __v.val[__j][__i];
}
static __inline__ int8x16x4_t vld1q_s8_x4(const int8_t *__p) {
  int8x16x4_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_s8_x4(int8_t *__p, int8x16x4_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ int8x16x4_t vld4q_s8(const int8_t *__p) {
  int8x16x4_t __r;
  for (int __i = 0; __i < 16; __i++)
    for (int __j = 0; __j < 4; __j++) __r.val[__j][__i] = __p[__i * 4 + __j];
  return __r;
}
static __inline__ void vst4q_s8(int8_t *__p, int8x16x4_t __v) {
  for (int __i = 0; __i < 16; __i++)
    for (int __j = 0; __j < 4; __j++) __p[__i * 4 + __j] = __v.val[__j][__i];
}
static __inline__ int16x8_t vld1q_s16(const int16_t *__p) {
  int16x8_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_s16(int16_t *__p, int16x8_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ int16x8_t vld1q_dup_s16(const int16_t *__p) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = *__p;
  return __r;
}
static __inline__ int16x8_t vld1q_lane_s16(const int16_t *__p, int16x8_t __v, const int __lane) {
  __v[__lane] = *__p;
  return __v;
}
static __inline__ void vst1q_lane_s16(int16_t *__p, int16x8_t __v, const int __lane) {
  *__p = __v[__lane];
}
static __inline__ int16x8x2_t vld1q_s16_x2(const int16_t *__p) {
  int16x8x2_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_s16_x2(int16_t *__p, int16x8x2_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ int16x8x2_t vld2q_s16(const int16_t *__p) {
  int16x8x2_t __r;
  for (int __i = 0; __i < 8; __i++)
    for (int __j = 0; __j < 2; __j++) __r.val[__j][__i] = __p[__i * 2 + __j];
  return __r;
}
static __inline__ void vst2q_s16(int16_t *__p, int16x8x2_t __v) {
  for (int __i = 0; __i < 8; __i++)
    for (int __j = 0; __j < 2; __j++) __p[__i * 2 + __j] = __v.val[__j][__i];
}
static __inline__ int16x8x3_t vld1q_s16_x3(const int16_t *__p) {
  int16x8x3_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_s16_x3(int16_t *__p, int16x8x3_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ int16x8x3_t vld3q_s16(const int16_t *__p) {
  int16x8x3_t __r;
  for (int __i = 0; __i < 8; __i++)
    for (int __j = 0; __j < 3; __j++) __r.val[__j][__i] = __p[__i * 3 + __j];
  return __r;
}
static __inline__ void vst3q_s16(int16_t *__p, int16x8x3_t __v) {
  for (int __i = 0; __i < 8; __i++)
    for (int __j = 0; __j < 3; __j++) __p[__i * 3 + __j] = __v.val[__j][__i];
}
static __inline__ int16x8x4_t vld1q_s16_x4(const int16_t *__p) {
  int16x8x4_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_s16_x4(int16_t *__p, int16x8x4_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ int16x8x4_t vld4q_s16(const int16_t *__p) {
  int16x8x4_t __r;
  for (int __i = 0; __i < 8; __i++)
    for (int __j = 0; __j < 4; __j++) __r.val[__j][__i] = __p[__i * 4 + __j];
  return __r;
}
static __inline__ void vst4q_s16(int16_t *__p, int16x8x4_t __v) {
  for (int __i = 0; __i < 8; __i++)
    for (int __j = 0; __j < 4; __j++) __p[__i * 4 + __j] = __v.val[__j][__i];
}
static __inline__ int32x4_t vld1q_s32(const int32_t *__p) {
  int32x4_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_s32(int32_t *__p, int32x4_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ int32x4_t vld1q_dup_s32(const int32_t *__p) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = *__p;
  return __r;
}
static __inline__ int32x4_t vld1q_lane_s32(const int32_t *__p, int32x4_t __v, const int __lane) {
  __v[__lane] = *__p;
  return __v;
}
static __inline__ void vst1q_lane_s32(int32_t *__p, int32x4_t __v, const int __lane) {
  *__p = __v[__lane];
}
static __inline__ int32x4x2_t vld1q_s32_x2(const int32_t *__p) {
  int32x4x2_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_s32_x2(int32_t *__p, int32x4x2_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ int32x4x2_t vld2q_s32(const int32_t *__p) {
  int32x4x2_t __r;
  for (int __i = 0; __i < 4; __i++)
    for (int __j = 0; __j < 2; __j++) __r.val[__j][__i] = __p[__i * 2 + __j];
  return __r;
}
static __inline__ void vst2q_s32(int32_t *__p, int32x4x2_t __v) {
  for (int __i = 0; __i < 4; __i++)
    for (int __j = 0; __j < 2; __j++) __p[__i * 2 + __j] = __v.val[__j][__i];
}
static __inline__ int32x4x3_t vld1q_s32_x3(const int32_t *__p) {
  int32x4x3_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_s32_x3(int32_t *__p, int32x4x3_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ int32x4x3_t vld3q_s32(const int32_t *__p) {
  int32x4x3_t __r;
  for (int __i = 0; __i < 4; __i++)
    for (int __j = 0; __j < 3; __j++) __r.val[__j][__i] = __p[__i * 3 + __j];
  return __r;
}
static __inline__ void vst3q_s32(int32_t *__p, int32x4x3_t __v) {
  for (int __i = 0; __i < 4; __i++)
    for (int __j = 0; __j < 3; __j++) __p[__i * 3 + __j] = __v.val[__j][__i];
}
static __inline__ int32x4x4_t vld1q_s32_x4(const int32_t *__p) {
  int32x4x4_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_s32_x4(int32_t *__p, int32x4x4_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ int32x4x4_t vld4q_s32(const int32_t *__p) {
  int32x4x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    for (int __j = 0; __j < 4; __j++) __r.val[__j][__i] = __p[__i * 4 + __j];
  return __r;
}
static __inline__ void vst4q_s32(int32_t *__p, int32x4x4_t __v) {
  for (int __i = 0; __i < 4; __i++)
    for (int __j = 0; __j < 4; __j++) __p[__i * 4 + __j] = __v.val[__j][__i];
}
static __inline__ int64x2_t vld1q_s64(const int64_t *__p) {
  int64x2_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_s64(int64_t *__p, int64x2_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ int64x2_t vld1q_dup_s64(const int64_t *__p) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = *__p;
  return __r;
}
static __inline__ int64x2_t vld1q_lane_s64(const int64_t *__p, int64x2_t __v, const int __lane) {
  __v[__lane] = *__p;
  return __v;
}
static __inline__ void vst1q_lane_s64(int64_t *__p, int64x2_t __v, const int __lane) {
  *__p = __v[__lane];
}
static __inline__ int64x2x2_t vld1q_s64_x2(const int64_t *__p) {
  int64x2x2_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_s64_x2(int64_t *__p, int64x2x2_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ int64x2x2_t vld2q_s64(const int64_t *__p) {
  int64x2x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 2; __j++) __r.val[__j][__i] = __p[__i * 2 + __j];
  return __r;
}
static __inline__ void vst2q_s64(int64_t *__p, int64x2x2_t __v) {
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 2; __j++) __p[__i * 2 + __j] = __v.val[__j][__i];
}
static __inline__ int64x2x3_t vld1q_s64_x3(const int64_t *__p) {
  int64x2x3_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_s64_x3(int64_t *__p, int64x2x3_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ int64x2x3_t vld3q_s64(const int64_t *__p) {
  int64x2x3_t __r;
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 3; __j++) __r.val[__j][__i] = __p[__i * 3 + __j];
  return __r;
}
static __inline__ void vst3q_s64(int64_t *__p, int64x2x3_t __v) {
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 3; __j++) __p[__i * 3 + __j] = __v.val[__j][__i];
}
static __inline__ int64x2x4_t vld1q_s64_x4(const int64_t *__p) {
  int64x2x4_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_s64_x4(int64_t *__p, int64x2x4_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ int64x2x4_t vld4q_s64(const int64_t *__p) {
  int64x2x4_t __r;
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 4; __j++) __r.val[__j][__i] = __p[__i * 4 + __j];
  return __r;
}
static __inline__ void vst4q_s64(int64_t *__p, int64x2x4_t __v) {
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 4; __j++) __p[__i * 4 + __j] = __v.val[__j][__i];
}
static __inline__ uint8x16_t vld1q_u8(const uint8_t *__p) {
  uint8x16_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_u8(uint8_t *__p, uint8x16_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ uint8x16_t vld1q_dup_u8(const uint8_t *__p) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = *__p;
  return __r;
}
static __inline__ uint8x16_t vld1q_lane_u8(const uint8_t *__p, uint8x16_t __v, const int __lane) {
  __v[__lane] = *__p;
  return __v;
}
static __inline__ void vst1q_lane_u8(uint8_t *__p, uint8x16_t __v, const int __lane) {
  *__p = __v[__lane];
}
static __inline__ uint8x16x2_t vld1q_u8_x2(const uint8_t *__p) {
  uint8x16x2_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_u8_x2(uint8_t *__p, uint8x16x2_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ uint8x16x2_t vld2q_u8(const uint8_t *__p) {
  uint8x16x2_t __r;
  for (int __i = 0; __i < 16; __i++)
    for (int __j = 0; __j < 2; __j++) __r.val[__j][__i] = __p[__i * 2 + __j];
  return __r;
}
static __inline__ void vst2q_u8(uint8_t *__p, uint8x16x2_t __v) {
  for (int __i = 0; __i < 16; __i++)
    for (int __j = 0; __j < 2; __j++) __p[__i * 2 + __j] = __v.val[__j][__i];
}
static __inline__ uint8x16x3_t vld1q_u8_x3(const uint8_t *__p) {
  uint8x16x3_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_u8_x3(uint8_t *__p, uint8x16x3_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ uint8x16x3_t vld3q_u8(const uint8_t *__p) {
  uint8x16x3_t __r;
  for (int __i = 0; __i < 16; __i++)
    for (int __j = 0; __j < 3; __j++) __r.val[__j][__i] = __p[__i * 3 + __j];
  return __r;
}
static __inline__ void vst3q_u8(uint8_t *__p, uint8x16x3_t __v) {
  for (int __i = 0; __i < 16; __i++)
    for (int __j = 0; __j < 3; __j++) __p[__i * 3 + __j] = __v.val[__j][__i];
}
static __inline__ uint8x16x4_t vld1q_u8_x4(const uint8_t *__p) {
  uint8x16x4_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_u8_x4(uint8_t *__p, uint8x16x4_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ uint8x16x4_t vld4q_u8(const uint8_t *__p) {
  uint8x16x4_t __r;
  for (int __i = 0; __i < 16; __i++)
    for (int __j = 0; __j < 4; __j++) __r.val[__j][__i] = __p[__i * 4 + __j];
  return __r;
}
static __inline__ void vst4q_u8(uint8_t *__p, uint8x16x4_t __v) {
  for (int __i = 0; __i < 16; __i++)
    for (int __j = 0; __j < 4; __j++) __p[__i * 4 + __j] = __v.val[__j][__i];
}
static __inline__ uint16x8_t vld1q_u16(const uint16_t *__p) {
  uint16x8_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_u16(uint16_t *__p, uint16x8_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ uint16x8_t vld1q_dup_u16(const uint16_t *__p) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = *__p;
  return __r;
}
static __inline__ uint16x8_t vld1q_lane_u16(const uint16_t *__p, uint16x8_t __v, const int __lane) {
  __v[__lane] = *__p;
  return __v;
}
static __inline__ void vst1q_lane_u16(uint16_t *__p, uint16x8_t __v, const int __lane) {
  *__p = __v[__lane];
}
static __inline__ uint16x8x2_t vld1q_u16_x2(const uint16_t *__p) {
  uint16x8x2_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_u16_x2(uint16_t *__p, uint16x8x2_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ uint16x8x2_t vld2q_u16(const uint16_t *__p) {
  uint16x8x2_t __r;
  for (int __i = 0; __i < 8; __i++)
    for (int __j = 0; __j < 2; __j++) __r.val[__j][__i] = __p[__i * 2 + __j];
  return __r;
}
static __inline__ void vst2q_u16(uint16_t *__p, uint16x8x2_t __v) {
  for (int __i = 0; __i < 8; __i++)
    for (int __j = 0; __j < 2; __j++) __p[__i * 2 + __j] = __v.val[__j][__i];
}
static __inline__ uint16x8x3_t vld1q_u16_x3(const uint16_t *__p) {
  uint16x8x3_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_u16_x3(uint16_t *__p, uint16x8x3_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ uint16x8x3_t vld3q_u16(const uint16_t *__p) {
  uint16x8x3_t __r;
  for (int __i = 0; __i < 8; __i++)
    for (int __j = 0; __j < 3; __j++) __r.val[__j][__i] = __p[__i * 3 + __j];
  return __r;
}
static __inline__ void vst3q_u16(uint16_t *__p, uint16x8x3_t __v) {
  for (int __i = 0; __i < 8; __i++)
    for (int __j = 0; __j < 3; __j++) __p[__i * 3 + __j] = __v.val[__j][__i];
}
static __inline__ uint16x8x4_t vld1q_u16_x4(const uint16_t *__p) {
  uint16x8x4_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_u16_x4(uint16_t *__p, uint16x8x4_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ uint16x8x4_t vld4q_u16(const uint16_t *__p) {
  uint16x8x4_t __r;
  for (int __i = 0; __i < 8; __i++)
    for (int __j = 0; __j < 4; __j++) __r.val[__j][__i] = __p[__i * 4 + __j];
  return __r;
}
static __inline__ void vst4q_u16(uint16_t *__p, uint16x8x4_t __v) {
  for (int __i = 0; __i < 8; __i++)
    for (int __j = 0; __j < 4; __j++) __p[__i * 4 + __j] = __v.val[__j][__i];
}
static __inline__ uint32x4_t vld1q_u32(const uint32_t *__p) {
  uint32x4_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_u32(uint32_t *__p, uint32x4_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ uint32x4_t vld1q_dup_u32(const uint32_t *__p) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = *__p;
  return __r;
}
static __inline__ uint32x4_t vld1q_lane_u32(const uint32_t *__p, uint32x4_t __v, const int __lane) {
  __v[__lane] = *__p;
  return __v;
}
static __inline__ void vst1q_lane_u32(uint32_t *__p, uint32x4_t __v, const int __lane) {
  *__p = __v[__lane];
}
static __inline__ uint32x4x2_t vld1q_u32_x2(const uint32_t *__p) {
  uint32x4x2_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_u32_x2(uint32_t *__p, uint32x4x2_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ uint32x4x2_t vld2q_u32(const uint32_t *__p) {
  uint32x4x2_t __r;
  for (int __i = 0; __i < 4; __i++)
    for (int __j = 0; __j < 2; __j++) __r.val[__j][__i] = __p[__i * 2 + __j];
  return __r;
}
static __inline__ void vst2q_u32(uint32_t *__p, uint32x4x2_t __v) {
  for (int __i = 0; __i < 4; __i++)
    for (int __j = 0; __j < 2; __j++) __p[__i * 2 + __j] = __v.val[__j][__i];
}
static __inline__ uint32x4x3_t vld1q_u32_x3(const uint32_t *__p) {
  uint32x4x3_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_u32_x3(uint32_t *__p, uint32x4x3_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ uint32x4x3_t vld3q_u32(const uint32_t *__p) {
  uint32x4x3_t __r;
  for (int __i = 0; __i < 4; __i++)
    for (int __j = 0; __j < 3; __j++) __r.val[__j][__i] = __p[__i * 3 + __j];
  return __r;
}
static __inline__ void vst3q_u32(uint32_t *__p, uint32x4x3_t __v) {
  for (int __i = 0; __i < 4; __i++)
    for (int __j = 0; __j < 3; __j++) __p[__i * 3 + __j] = __v.val[__j][__i];
}
static __inline__ uint32x4x4_t vld1q_u32_x4(const uint32_t *__p) {
  uint32x4x4_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_u32_x4(uint32_t *__p, uint32x4x4_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ uint32x4x4_t vld4q_u32(const uint32_t *__p) {
  uint32x4x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    for (int __j = 0; __j < 4; __j++) __r.val[__j][__i] = __p[__i * 4 + __j];
  return __r;
}
static __inline__ void vst4q_u32(uint32_t *__p, uint32x4x4_t __v) {
  for (int __i = 0; __i < 4; __i++)
    for (int __j = 0; __j < 4; __j++) __p[__i * 4 + __j] = __v.val[__j][__i];
}
static __inline__ uint64x2_t vld1q_u64(const uint64_t *__p) {
  uint64x2_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_u64(uint64_t *__p, uint64x2_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ uint64x2_t vld1q_dup_u64(const uint64_t *__p) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = *__p;
  return __r;
}
static __inline__ uint64x2_t vld1q_lane_u64(const uint64_t *__p, uint64x2_t __v, const int __lane) {
  __v[__lane] = *__p;
  return __v;
}
static __inline__ void vst1q_lane_u64(uint64_t *__p, uint64x2_t __v, const int __lane) {
  *__p = __v[__lane];
}
static __inline__ uint64x2x2_t vld1q_u64_x2(const uint64_t *__p) {
  uint64x2x2_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_u64_x2(uint64_t *__p, uint64x2x2_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ uint64x2x2_t vld2q_u64(const uint64_t *__p) {
  uint64x2x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 2; __j++) __r.val[__j][__i] = __p[__i * 2 + __j];
  return __r;
}
static __inline__ void vst2q_u64(uint64_t *__p, uint64x2x2_t __v) {
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 2; __j++) __p[__i * 2 + __j] = __v.val[__j][__i];
}
static __inline__ uint64x2x3_t vld1q_u64_x3(const uint64_t *__p) {
  uint64x2x3_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_u64_x3(uint64_t *__p, uint64x2x3_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ uint64x2x3_t vld3q_u64(const uint64_t *__p) {
  uint64x2x3_t __r;
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 3; __j++) __r.val[__j][__i] = __p[__i * 3 + __j];
  return __r;
}
static __inline__ void vst3q_u64(uint64_t *__p, uint64x2x3_t __v) {
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 3; __j++) __p[__i * 3 + __j] = __v.val[__j][__i];
}
static __inline__ uint64x2x4_t vld1q_u64_x4(const uint64_t *__p) {
  uint64x2x4_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_u64_x4(uint64_t *__p, uint64x2x4_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ uint64x2x4_t vld4q_u64(const uint64_t *__p) {
  uint64x2x4_t __r;
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 4; __j++) __r.val[__j][__i] = __p[__i * 4 + __j];
  return __r;
}
static __inline__ void vst4q_u64(uint64_t *__p, uint64x2x4_t __v) {
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 4; __j++) __p[__i * 4 + __j] = __v.val[__j][__i];
}
static __inline__ float32x4_t vld1q_f32(const float32_t *__p) {
  float32x4_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_f32(float32_t *__p, float32x4_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ float32x4_t vld1q_dup_f32(const float32_t *__p) {
  float32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = *__p;
  return __r;
}
static __inline__ float32x4_t vld1q_lane_f32(const float32_t *__p, float32x4_t __v,
    const int __lane) {
  __v[__lane] = *__p;
  return __v;
}
static __inline__ void vst1q_lane_f32(float32_t *__p, float32x4_t __v, const int __lane) {
  *__p = __v[__lane];
}
static __inline__ float32x4x2_t vld1q_f32_x2(const float32_t *__p) {
  float32x4x2_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_f32_x2(float32_t *__p, float32x4x2_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ float32x4x2_t vld2q_f32(const float32_t *__p) {
  float32x4x2_t __r;
  for (int __i = 0; __i < 4; __i++)
    for (int __j = 0; __j < 2; __j++) __r.val[__j][__i] = __p[__i * 2 + __j];
  return __r;
}
static __inline__ void vst2q_f32(float32_t *__p, float32x4x2_t __v) {
  for (int __i = 0; __i < 4; __i++)
    for (int __j = 0; __j < 2; __j++) __p[__i * 2 + __j] = __v.val[__j][__i];
}
static __inline__ float32x4x3_t vld1q_f32_x3(const float32_t *__p) {
  float32x4x3_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_f32_x3(float32_t *__p, float32x4x3_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ float32x4x3_t vld3q_f32(const float32_t *__p) {
  float32x4x3_t __r;
  for (int __i = 0; __i < 4; __i++)
    for (int __j = 0; __j < 3; __j++) __r.val[__j][__i] = __p[__i * 3 + __j];
  return __r;
}
static __inline__ void vst3q_f32(float32_t *__p, float32x4x3_t __v) {
  for (int __i = 0; __i < 4; __i++)
    for (int __j = 0; __j < 3; __j++) __p[__i * 3 + __j] = __v.val[__j][__i];
}
static __inline__ float32x4x4_t vld1q_f32_x4(const float32_t *__p) {
  float32x4x4_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_f32_x4(float32_t *__p, float32x4x4_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ float32x4x4_t vld4q_f32(const float32_t *__p) {
  float32x4x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    for (int __j = 0; __j < 4; __j++) __r.val[__j][__i] = __p[__i * 4 + __j];
  return __r;
}
static __inline__ void vst4q_f32(float32_t *__p, float32x4x4_t __v) {
  for (int __i = 0; __i < 4; __i++)
    for (int __j = 0; __j < 4; __j++) __p[__i * 4 + __j] = __v.val[__j][__i];
}
static __inline__ float64x2_t vld1q_f64(const float64_t *__p) {
  float64x2_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_f64(float64_t *__p, float64x2_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ float64x2_t vld1q_dup_f64(const float64_t *__p) {
  float64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = *__p;
  return __r;
}
static __inline__ float64x2_t vld1q_lane_f64(const float64_t *__p, float64x2_t __v,
    const int __lane) {
  __v[__lane] = *__p;
  return __v;
}
static __inline__ void vst1q_lane_f64(float64_t *__p, float64x2_t __v, const int __lane) {
  *__p = __v[__lane];
}
static __inline__ float64x2x2_t vld1q_f64_x2(const float64_t *__p) {
  float64x2x2_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_f64_x2(float64_t *__p, float64x2x2_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ float64x2x2_t vld2q_f64(const float64_t *__p) {
  float64x2x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 2; __j++) __r.val[__j][__i] = __p[__i * 2 + __j];
  return __r;
}
static __inline__ void vst2q_f64(float64_t *__p, float64x2x2_t __v) {
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 2; __j++) __p[__i * 2 + __j] = __v.val[__j][__i];
}
static __inline__ float64x2x3_t vld1q_f64_x3(const float64_t *__p) {
  float64x2x3_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_f64_x3(float64_t *__p, float64x2x3_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ float64x2x3_t vld3q_f64(const float64_t *__p) {
  float64x2x3_t __r;
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 3; __j++) __r.val[__j][__i] = __p[__i * 3 + __j];
  return __r;
}
static __inline__ void vst3q_f64(float64_t *__p, float64x2x3_t __v) {
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 3; __j++) __p[__i * 3 + __j] = __v.val[__j][__i];
}
static __inline__ float64x2x4_t vld1q_f64_x4(const float64_t *__p) {
  float64x2x4_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_f64_x4(float64_t *__p, float64x2x4_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ float64x2x4_t vld4q_f64(const float64_t *__p) {
  float64x2x4_t __r;
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 4; __j++) __r.val[__j][__i] = __p[__i * 4 + __j];
  return __r;
}
static __inline__ void vst4q_f64(float64_t *__p, float64x2x4_t __v) {
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 4; __j++) __p[__i * 4 + __j] = __v.val[__j][__i];
}
static __inline__ poly8x16_t vld1q_p8(const poly8_t *__p) {
  poly8x16_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_p8(poly8_t *__p, poly8x16_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ poly8x16_t vld1q_dup_p8(const poly8_t *__p) {
  poly8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = *__p;
  return __r;
}
static __inline__ poly8x16_t vld1q_lane_p8(const poly8_t *__p, poly8x16_t __v, const int __lane) {
  __v[__lane] = *__p;
  return __v;
}
static __inline__ void vst1q_lane_p8(poly8_t *__p, poly8x16_t __v, const int __lane) {
  *__p = __v[__lane];
}
static __inline__ poly8x16x2_t vld1q_p8_x2(const poly8_t *__p) {
  poly8x16x2_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_p8_x2(poly8_t *__p, poly8x16x2_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ poly8x16x2_t vld2q_p8(const poly8_t *__p) {
  poly8x16x2_t __r;
  for (int __i = 0; __i < 16; __i++)
    for (int __j = 0; __j < 2; __j++) __r.val[__j][__i] = __p[__i * 2 + __j];
  return __r;
}
static __inline__ void vst2q_p8(poly8_t *__p, poly8x16x2_t __v) {
  for (int __i = 0; __i < 16; __i++)
    for (int __j = 0; __j < 2; __j++) __p[__i * 2 + __j] = __v.val[__j][__i];
}
static __inline__ poly8x16x3_t vld1q_p8_x3(const poly8_t *__p) {
  poly8x16x3_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_p8_x3(poly8_t *__p, poly8x16x3_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ poly8x16x3_t vld3q_p8(const poly8_t *__p) {
  poly8x16x3_t __r;
  for (int __i = 0; __i < 16; __i++)
    for (int __j = 0; __j < 3; __j++) __r.val[__j][__i] = __p[__i * 3 + __j];
  return __r;
}
static __inline__ void vst3q_p8(poly8_t *__p, poly8x16x3_t __v) {
  for (int __i = 0; __i < 16; __i++)
    for (int __j = 0; __j < 3; __j++) __p[__i * 3 + __j] = __v.val[__j][__i];
}
static __inline__ poly8x16x4_t vld1q_p8_x4(const poly8_t *__p) {
  poly8x16x4_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_p8_x4(poly8_t *__p, poly8x16x4_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ poly8x16x4_t vld4q_p8(const poly8_t *__p) {
  poly8x16x4_t __r;
  for (int __i = 0; __i < 16; __i++)
    for (int __j = 0; __j < 4; __j++) __r.val[__j][__i] = __p[__i * 4 + __j];
  return __r;
}
static __inline__ void vst4q_p8(poly8_t *__p, poly8x16x4_t __v) {
  for (int __i = 0; __i < 16; __i++)
    for (int __j = 0; __j < 4; __j++) __p[__i * 4 + __j] = __v.val[__j][__i];
}
static __inline__ poly16x8_t vld1q_p16(const poly16_t *__p) {
  poly16x8_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_p16(poly16_t *__p, poly16x8_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ poly16x8_t vld1q_dup_p16(const poly16_t *__p) {
  poly16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = *__p;
  return __r;
}
static __inline__ poly16x8_t vld1q_lane_p16(const poly16_t *__p, poly16x8_t __v, const int __lane) {
  __v[__lane] = *__p;
  return __v;
}
static __inline__ void vst1q_lane_p16(poly16_t *__p, poly16x8_t __v, const int __lane) {
  *__p = __v[__lane];
}
static __inline__ poly16x8x2_t vld1q_p16_x2(const poly16_t *__p) {
  poly16x8x2_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_p16_x2(poly16_t *__p, poly16x8x2_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ poly16x8x2_t vld2q_p16(const poly16_t *__p) {
  poly16x8x2_t __r;
  for (int __i = 0; __i < 8; __i++)
    for (int __j = 0; __j < 2; __j++) __r.val[__j][__i] = __p[__i * 2 + __j];
  return __r;
}
static __inline__ void vst2q_p16(poly16_t *__p, poly16x8x2_t __v) {
  for (int __i = 0; __i < 8; __i++)
    for (int __j = 0; __j < 2; __j++) __p[__i * 2 + __j] = __v.val[__j][__i];
}
static __inline__ poly16x8x3_t vld1q_p16_x3(const poly16_t *__p) {
  poly16x8x3_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_p16_x3(poly16_t *__p, poly16x8x3_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ poly16x8x3_t vld3q_p16(const poly16_t *__p) {
  poly16x8x3_t __r;
  for (int __i = 0; __i < 8; __i++)
    for (int __j = 0; __j < 3; __j++) __r.val[__j][__i] = __p[__i * 3 + __j];
  return __r;
}
static __inline__ void vst3q_p16(poly16_t *__p, poly16x8x3_t __v) {
  for (int __i = 0; __i < 8; __i++)
    for (int __j = 0; __j < 3; __j++) __p[__i * 3 + __j] = __v.val[__j][__i];
}
static __inline__ poly16x8x4_t vld1q_p16_x4(const poly16_t *__p) {
  poly16x8x4_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_p16_x4(poly16_t *__p, poly16x8x4_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ poly16x8x4_t vld4q_p16(const poly16_t *__p) {
  poly16x8x4_t __r;
  for (int __i = 0; __i < 8; __i++)
    for (int __j = 0; __j < 4; __j++) __r.val[__j][__i] = __p[__i * 4 + __j];
  return __r;
}
static __inline__ void vst4q_p16(poly16_t *__p, poly16x8x4_t __v) {
  for (int __i = 0; __i < 8; __i++)
    for (int __j = 0; __j < 4; __j++) __p[__i * 4 + __j] = __v.val[__j][__i];
}
static __inline__ poly64x2_t vld1q_p64(const poly64_t *__p) {
  poly64x2_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_p64(poly64_t *__p, poly64x2_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ poly64x2_t vld1q_dup_p64(const poly64_t *__p) {
  poly64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = *__p;
  return __r;
}
static __inline__ poly64x2_t vld1q_lane_p64(const poly64_t *__p, poly64x2_t __v, const int __lane) {
  __v[__lane] = *__p;
  return __v;
}
static __inline__ void vst1q_lane_p64(poly64_t *__p, poly64x2_t __v, const int __lane) {
  *__p = __v[__lane];
}
static __inline__ poly64x2x2_t vld1q_p64_x2(const poly64_t *__p) {
  poly64x2x2_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_p64_x2(poly64_t *__p, poly64x2x2_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ poly64x2x2_t vld2q_p64(const poly64_t *__p) {
  poly64x2x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 2; __j++) __r.val[__j][__i] = __p[__i * 2 + __j];
  return __r;
}
static __inline__ void vst2q_p64(poly64_t *__p, poly64x2x2_t __v) {
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 2; __j++) __p[__i * 2 + __j] = __v.val[__j][__i];
}
static __inline__ poly64x2x3_t vld1q_p64_x3(const poly64_t *__p) {
  poly64x2x3_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_p64_x3(poly64_t *__p, poly64x2x3_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ poly64x2x3_t vld3q_p64(const poly64_t *__p) {
  poly64x2x3_t __r;
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 3; __j++) __r.val[__j][__i] = __p[__i * 3 + __j];
  return __r;
}
static __inline__ void vst3q_p64(poly64_t *__p, poly64x2x3_t __v) {
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 3; __j++) __p[__i * 3 + __j] = __v.val[__j][__i];
}
static __inline__ poly64x2x4_t vld1q_p64_x4(const poly64_t *__p) {
  poly64x2x4_t __r;
  __builtin_memcpy(&__r, __p, sizeof __r);
  return __r;
}
static __inline__ void vst1q_p64_x4(poly64_t *__p, poly64x2x4_t __v) {
  __builtin_memcpy(__p, &__v, sizeof __v);
}
static __inline__ poly64x2x4_t vld4q_p64(const poly64_t *__p) {
  poly64x2x4_t __r;
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 4; __j++) __r.val[__j][__i] = __p[__i * 4 + __j];
  return __r;
}
static __inline__ void vst4q_p64(poly64_t *__p, poly64x2x4_t __v) {
  for (int __i = 0; __i < 2; __i++)
    for (int __j = 0; __j < 4; __j++) __p[__i * 4 + __j] = __v.val[__j][__i];
}

/* Building a vector and taking one apart. */
static __inline__ int8x8_t vdup_n_s8(int8_t __x) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ int8x8_t vmov_n_s8(int8_t __x) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ int8_t vget_lane_s8(int8x8_t __v, const int __lane) {
  return __v[__lane];
}
static __inline__ int8x8_t vset_lane_s8(int8_t __x, int8x8_t __v, const int __lane) {
  __v[__lane] = __x;
  return __v;
}
static __inline__ int8x8_t vdup_lane_s8(int8x8_t __v, const int __lane) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ int8x8_t vdup_laneq_s8(int8x16_t __v, const int __lane) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ int8x8_t vcopy_lane_s8(int8x8_t __a, const int __la, int8x8_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ int8x8_t vcopy_laneq_s8(int8x8_t __a, const int __la, int8x16_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ int16x4_t vdup_n_s16(int16_t __x) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ int16x4_t vmov_n_s16(int16_t __x) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ int16_t vget_lane_s16(int16x4_t __v, const int __lane) {
  return __v[__lane];
}
static __inline__ int16x4_t vset_lane_s16(int16_t __x, int16x4_t __v, const int __lane) {
  __v[__lane] = __x;
  return __v;
}
static __inline__ int16x4_t vdup_lane_s16(int16x4_t __v, const int __lane) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ int16x4_t vdup_laneq_s16(int16x8_t __v, const int __lane) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ int16x4_t vcopy_lane_s16(int16x4_t __a, const int __la, int16x4_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ int16x4_t vcopy_laneq_s16(int16x4_t __a, const int __la, int16x8_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ int32x2_t vdup_n_s32(int32_t __x) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ int32x2_t vmov_n_s32(int32_t __x) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ int32_t vget_lane_s32(int32x2_t __v, const int __lane) {
  return __v[__lane];
}
static __inline__ int32x2_t vset_lane_s32(int32_t __x, int32x2_t __v, const int __lane) {
  __v[__lane] = __x;
  return __v;
}
static __inline__ int32x2_t vdup_lane_s32(int32x2_t __v, const int __lane) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ int32x2_t vdup_laneq_s32(int32x4_t __v, const int __lane) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ int32x2_t vcopy_lane_s32(int32x2_t __a, const int __la, int32x2_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ int32x2_t vcopy_laneq_s32(int32x2_t __a, const int __la, int32x4_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ int64x1_t vdup_n_s64(int64_t __x) {
  int64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ int64x1_t vmov_n_s64(int64_t __x) {
  int64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ int64_t vget_lane_s64(int64x1_t __v, const int __lane) {
  return __v[__lane];
}
static __inline__ int64x1_t vset_lane_s64(int64_t __x, int64x1_t __v, const int __lane) {
  __v[__lane] = __x;
  return __v;
}
static __inline__ int64x1_t vdup_lane_s64(int64x1_t __v, const int __lane) {
  int64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ int64x1_t vdup_laneq_s64(int64x2_t __v, const int __lane) {
  int64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ int64x1_t vcopy_lane_s64(int64x1_t __a, const int __la, int64x1_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ int64x1_t vcopy_laneq_s64(int64x1_t __a, const int __la, int64x2_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ uint8x8_t vdup_n_u8(uint8_t __x) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ uint8x8_t vmov_n_u8(uint8_t __x) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ uint8_t vget_lane_u8(uint8x8_t __v, const int __lane) {
  return __v[__lane];
}
static __inline__ uint8x8_t vset_lane_u8(uint8_t __x, uint8x8_t __v, const int __lane) {
  __v[__lane] = __x;
  return __v;
}
static __inline__ uint8x8_t vdup_lane_u8(uint8x8_t __v, const int __lane) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ uint8x8_t vdup_laneq_u8(uint8x16_t __v, const int __lane) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ uint8x8_t vcopy_lane_u8(uint8x8_t __a, const int __la, uint8x8_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ uint8x8_t vcopy_laneq_u8(uint8x8_t __a, const int __la, uint8x16_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ uint16x4_t vdup_n_u16(uint16_t __x) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ uint16x4_t vmov_n_u16(uint16_t __x) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ uint16_t vget_lane_u16(uint16x4_t __v, const int __lane) {
  return __v[__lane];
}
static __inline__ uint16x4_t vset_lane_u16(uint16_t __x, uint16x4_t __v, const int __lane) {
  __v[__lane] = __x;
  return __v;
}
static __inline__ uint16x4_t vdup_lane_u16(uint16x4_t __v, const int __lane) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ uint16x4_t vdup_laneq_u16(uint16x8_t __v, const int __lane) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ uint16x4_t vcopy_lane_u16(uint16x4_t __a, const int __la, uint16x4_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ uint16x4_t vcopy_laneq_u16(uint16x4_t __a, const int __la, uint16x8_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ uint32x2_t vdup_n_u32(uint32_t __x) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ uint32x2_t vmov_n_u32(uint32_t __x) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ uint32_t vget_lane_u32(uint32x2_t __v, const int __lane) {
  return __v[__lane];
}
static __inline__ uint32x2_t vset_lane_u32(uint32_t __x, uint32x2_t __v, const int __lane) {
  __v[__lane] = __x;
  return __v;
}
static __inline__ uint32x2_t vdup_lane_u32(uint32x2_t __v, const int __lane) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ uint32x2_t vdup_laneq_u32(uint32x4_t __v, const int __lane) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ uint32x2_t vcopy_lane_u32(uint32x2_t __a, const int __la, uint32x2_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ uint32x2_t vcopy_laneq_u32(uint32x2_t __a, const int __la, uint32x4_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ uint64x1_t vdup_n_u64(uint64_t __x) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ uint64x1_t vmov_n_u64(uint64_t __x) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ uint64_t vget_lane_u64(uint64x1_t __v, const int __lane) {
  return __v[__lane];
}
static __inline__ uint64x1_t vset_lane_u64(uint64_t __x, uint64x1_t __v, const int __lane) {
  __v[__lane] = __x;
  return __v;
}
static __inline__ uint64x1_t vdup_lane_u64(uint64x1_t __v, const int __lane) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ uint64x1_t vdup_laneq_u64(uint64x2_t __v, const int __lane) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ uint64x1_t vcopy_lane_u64(uint64x1_t __a, const int __la, uint64x1_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ uint64x1_t vcopy_laneq_u64(uint64x1_t __a, const int __la, uint64x2_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ float32x2_t vdup_n_f32(float32_t __x) {
  float32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ float32x2_t vmov_n_f32(float32_t __x) {
  float32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ float32_t vget_lane_f32(float32x2_t __v, const int __lane) {
  return __v[__lane];
}
static __inline__ float32x2_t vset_lane_f32(float32_t __x, float32x2_t __v, const int __lane) {
  __v[__lane] = __x;
  return __v;
}
static __inline__ float32x2_t vdup_lane_f32(float32x2_t __v, const int __lane) {
  float32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ float32x2_t vdup_laneq_f32(float32x4_t __v, const int __lane) {
  float32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ float32x2_t vcopy_lane_f32(float32x2_t __a, const int __la, float32x2_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ float32x2_t vcopy_laneq_f32(float32x2_t __a, const int __la, float32x4_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ float64x1_t vdup_n_f64(float64_t __x) {
  float64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ float64x1_t vmov_n_f64(float64_t __x) {
  float64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ float64_t vget_lane_f64(float64x1_t __v, const int __lane) {
  return __v[__lane];
}
static __inline__ float64x1_t vset_lane_f64(float64_t __x, float64x1_t __v, const int __lane) {
  __v[__lane] = __x;
  return __v;
}
static __inline__ float64x1_t vdup_lane_f64(float64x1_t __v, const int __lane) {
  float64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ float64x1_t vdup_laneq_f64(float64x2_t __v, const int __lane) {
  float64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ float64x1_t vcopy_lane_f64(float64x1_t __a, const int __la, float64x1_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ float64x1_t vcopy_laneq_f64(float64x1_t __a, const int __la, float64x2_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ poly8x8_t vdup_n_p8(poly8_t __x) {
  poly8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ poly8x8_t vmov_n_p8(poly8_t __x) {
  poly8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ poly8_t vget_lane_p8(poly8x8_t __v, const int __lane) {
  return __v[__lane];
}
static __inline__ poly8x8_t vset_lane_p8(poly8_t __x, poly8x8_t __v, const int __lane) {
  __v[__lane] = __x;
  return __v;
}
static __inline__ poly8x8_t vdup_lane_p8(poly8x8_t __v, const int __lane) {
  poly8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ poly8x8_t vdup_laneq_p8(poly8x16_t __v, const int __lane) {
  poly8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ poly8x8_t vcopy_lane_p8(poly8x8_t __a, const int __la, poly8x8_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ poly8x8_t vcopy_laneq_p8(poly8x8_t __a, const int __la, poly8x16_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ poly16x4_t vdup_n_p16(poly16_t __x) {
  poly16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ poly16x4_t vmov_n_p16(poly16_t __x) {
  poly16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ poly16_t vget_lane_p16(poly16x4_t __v, const int __lane) {
  return __v[__lane];
}
static __inline__ poly16x4_t vset_lane_p16(poly16_t __x, poly16x4_t __v, const int __lane) {
  __v[__lane] = __x;
  return __v;
}
static __inline__ poly16x4_t vdup_lane_p16(poly16x4_t __v, const int __lane) {
  poly16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ poly16x4_t vdup_laneq_p16(poly16x8_t __v, const int __lane) {
  poly16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ poly16x4_t vcopy_lane_p16(poly16x4_t __a, const int __la, poly16x4_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ poly16x4_t vcopy_laneq_p16(poly16x4_t __a, const int __la, poly16x8_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ poly64x1_t vdup_n_p64(poly64_t __x) {
  poly64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ poly64x1_t vmov_n_p64(poly64_t __x) {
  poly64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ poly64_t vget_lane_p64(poly64x1_t __v, const int __lane) {
  return __v[__lane];
}
static __inline__ poly64x1_t vset_lane_p64(poly64_t __x, poly64x1_t __v, const int __lane) {
  __v[__lane] = __x;
  return __v;
}
static __inline__ poly64x1_t vdup_lane_p64(poly64x1_t __v, const int __lane) {
  poly64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ poly64x1_t vdup_laneq_p64(poly64x2_t __v, const int __lane) {
  poly64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ poly64x1_t vcopy_lane_p64(poly64x1_t __a, const int __la, poly64x1_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ poly64x1_t vcopy_laneq_p64(poly64x1_t __a, const int __la, poly64x2_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ int8x16_t vdupq_n_s8(int8_t __x) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ int8x16_t vmovq_n_s8(int8_t __x) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ int8_t vgetq_lane_s8(int8x16_t __v, const int __lane) {
  return __v[__lane];
}
static __inline__ int8x16_t vsetq_lane_s8(int8_t __x, int8x16_t __v, const int __lane) {
  __v[__lane] = __x;
  return __v;
}
static __inline__ int8x16_t vdupq_lane_s8(int8x8_t __v, const int __lane) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ int8x16_t vdupq_laneq_s8(int8x16_t __v, const int __lane) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ int8x16_t vcopyq_lane_s8(int8x16_t __a, const int __la, int8x8_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ int8x16_t vcopyq_laneq_s8(int8x16_t __a, const int __la, int8x16_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ int16x8_t vdupq_n_s16(int16_t __x) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ int16x8_t vmovq_n_s16(int16_t __x) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ int16_t vgetq_lane_s16(int16x8_t __v, const int __lane) {
  return __v[__lane];
}
static __inline__ int16x8_t vsetq_lane_s16(int16_t __x, int16x8_t __v, const int __lane) {
  __v[__lane] = __x;
  return __v;
}
static __inline__ int16x8_t vdupq_lane_s16(int16x4_t __v, const int __lane) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ int16x8_t vdupq_laneq_s16(int16x8_t __v, const int __lane) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ int16x8_t vcopyq_lane_s16(int16x8_t __a, const int __la, int16x4_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ int16x8_t vcopyq_laneq_s16(int16x8_t __a, const int __la, int16x8_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ int32x4_t vdupq_n_s32(int32_t __x) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ int32x4_t vmovq_n_s32(int32_t __x) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ int32_t vgetq_lane_s32(int32x4_t __v, const int __lane) {
  return __v[__lane];
}
static __inline__ int32x4_t vsetq_lane_s32(int32_t __x, int32x4_t __v, const int __lane) {
  __v[__lane] = __x;
  return __v;
}
static __inline__ int32x4_t vdupq_lane_s32(int32x2_t __v, const int __lane) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ int32x4_t vdupq_laneq_s32(int32x4_t __v, const int __lane) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ int32x4_t vcopyq_lane_s32(int32x4_t __a, const int __la, int32x2_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ int32x4_t vcopyq_laneq_s32(int32x4_t __a, const int __la, int32x4_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ int64x2_t vdupq_n_s64(int64_t __x) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ int64x2_t vmovq_n_s64(int64_t __x) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ int64_t vgetq_lane_s64(int64x2_t __v, const int __lane) {
  return __v[__lane];
}
static __inline__ int64x2_t vsetq_lane_s64(int64_t __x, int64x2_t __v, const int __lane) {
  __v[__lane] = __x;
  return __v;
}
static __inline__ int64x2_t vdupq_lane_s64(int64x1_t __v, const int __lane) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ int64x2_t vdupq_laneq_s64(int64x2_t __v, const int __lane) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ int64x2_t vcopyq_lane_s64(int64x2_t __a, const int __la, int64x1_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ int64x2_t vcopyq_laneq_s64(int64x2_t __a, const int __la, int64x2_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ uint8x16_t vdupq_n_u8(uint8_t __x) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ uint8x16_t vmovq_n_u8(uint8_t __x) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ uint8_t vgetq_lane_u8(uint8x16_t __v, const int __lane) {
  return __v[__lane];
}
static __inline__ uint8x16_t vsetq_lane_u8(uint8_t __x, uint8x16_t __v, const int __lane) {
  __v[__lane] = __x;
  return __v;
}
static __inline__ uint8x16_t vdupq_lane_u8(uint8x8_t __v, const int __lane) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ uint8x16_t vdupq_laneq_u8(uint8x16_t __v, const int __lane) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ uint8x16_t vcopyq_lane_u8(uint8x16_t __a, const int __la, uint8x8_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ uint8x16_t vcopyq_laneq_u8(uint8x16_t __a, const int __la, uint8x16_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ uint16x8_t vdupq_n_u16(uint16_t __x) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ uint16x8_t vmovq_n_u16(uint16_t __x) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ uint16_t vgetq_lane_u16(uint16x8_t __v, const int __lane) {
  return __v[__lane];
}
static __inline__ uint16x8_t vsetq_lane_u16(uint16_t __x, uint16x8_t __v, const int __lane) {
  __v[__lane] = __x;
  return __v;
}
static __inline__ uint16x8_t vdupq_lane_u16(uint16x4_t __v, const int __lane) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ uint16x8_t vdupq_laneq_u16(uint16x8_t __v, const int __lane) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ uint16x8_t vcopyq_lane_u16(uint16x8_t __a, const int __la, uint16x4_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ uint16x8_t vcopyq_laneq_u16(uint16x8_t __a, const int __la, uint16x8_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ uint32x4_t vdupq_n_u32(uint32_t __x) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ uint32x4_t vmovq_n_u32(uint32_t __x) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ uint32_t vgetq_lane_u32(uint32x4_t __v, const int __lane) {
  return __v[__lane];
}
static __inline__ uint32x4_t vsetq_lane_u32(uint32_t __x, uint32x4_t __v, const int __lane) {
  __v[__lane] = __x;
  return __v;
}
static __inline__ uint32x4_t vdupq_lane_u32(uint32x2_t __v, const int __lane) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ uint32x4_t vdupq_laneq_u32(uint32x4_t __v, const int __lane) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ uint32x4_t vcopyq_lane_u32(uint32x4_t __a, const int __la, uint32x2_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ uint32x4_t vcopyq_laneq_u32(uint32x4_t __a, const int __la, uint32x4_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ uint64x2_t vdupq_n_u64(uint64_t __x) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ uint64x2_t vmovq_n_u64(uint64_t __x) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ uint64_t vgetq_lane_u64(uint64x2_t __v, const int __lane) {
  return __v[__lane];
}
static __inline__ uint64x2_t vsetq_lane_u64(uint64_t __x, uint64x2_t __v, const int __lane) {
  __v[__lane] = __x;
  return __v;
}
static __inline__ uint64x2_t vdupq_lane_u64(uint64x1_t __v, const int __lane) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ uint64x2_t vdupq_laneq_u64(uint64x2_t __v, const int __lane) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ uint64x2_t vcopyq_lane_u64(uint64x2_t __a, const int __la, uint64x1_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ uint64x2_t vcopyq_laneq_u64(uint64x2_t __a, const int __la, uint64x2_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ float32x4_t vdupq_n_f32(float32_t __x) {
  float32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ float32x4_t vmovq_n_f32(float32_t __x) {
  float32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ float32_t vgetq_lane_f32(float32x4_t __v, const int __lane) {
  return __v[__lane];
}
static __inline__ float32x4_t vsetq_lane_f32(float32_t __x, float32x4_t __v, const int __lane) {
  __v[__lane] = __x;
  return __v;
}
static __inline__ float32x4_t vdupq_lane_f32(float32x2_t __v, const int __lane) {
  float32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ float32x4_t vdupq_laneq_f32(float32x4_t __v, const int __lane) {
  float32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ float32x4_t vcopyq_lane_f32(float32x4_t __a, const int __la, float32x2_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ float32x4_t vcopyq_laneq_f32(float32x4_t __a, const int __la, float32x4_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ float64x2_t vdupq_n_f64(float64_t __x) {
  float64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ float64x2_t vmovq_n_f64(float64_t __x) {
  float64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ float64_t vgetq_lane_f64(float64x2_t __v, const int __lane) {
  return __v[__lane];
}
static __inline__ float64x2_t vsetq_lane_f64(float64_t __x, float64x2_t __v, const int __lane) {
  __v[__lane] = __x;
  return __v;
}
static __inline__ float64x2_t vdupq_lane_f64(float64x1_t __v, const int __lane) {
  float64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ float64x2_t vdupq_laneq_f64(float64x2_t __v, const int __lane) {
  float64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ float64x2_t vcopyq_lane_f64(float64x2_t __a, const int __la, float64x1_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ float64x2_t vcopyq_laneq_f64(float64x2_t __a, const int __la, float64x2_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ poly8x16_t vdupq_n_p8(poly8_t __x) {
  poly8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ poly8x16_t vmovq_n_p8(poly8_t __x) {
  poly8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ poly8_t vgetq_lane_p8(poly8x16_t __v, const int __lane) {
  return __v[__lane];
}
static __inline__ poly8x16_t vsetq_lane_p8(poly8_t __x, poly8x16_t __v, const int __lane) {
  __v[__lane] = __x;
  return __v;
}
static __inline__ poly8x16_t vdupq_lane_p8(poly8x8_t __v, const int __lane) {
  poly8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ poly8x16_t vdupq_laneq_p8(poly8x16_t __v, const int __lane) {
  poly8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ poly8x16_t vcopyq_lane_p8(poly8x16_t __a, const int __la, poly8x8_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ poly8x16_t vcopyq_laneq_p8(poly8x16_t __a, const int __la, poly8x16_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ poly16x8_t vdupq_n_p16(poly16_t __x) {
  poly16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ poly16x8_t vmovq_n_p16(poly16_t __x) {
  poly16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ poly16_t vgetq_lane_p16(poly16x8_t __v, const int __lane) {
  return __v[__lane];
}
static __inline__ poly16x8_t vsetq_lane_p16(poly16_t __x, poly16x8_t __v, const int __lane) {
  __v[__lane] = __x;
  return __v;
}
static __inline__ poly16x8_t vdupq_lane_p16(poly16x4_t __v, const int __lane) {
  poly16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ poly16x8_t vdupq_laneq_p16(poly16x8_t __v, const int __lane) {
  poly16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ poly16x8_t vcopyq_lane_p16(poly16x8_t __a, const int __la, poly16x4_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ poly16x8_t vcopyq_laneq_p16(poly16x8_t __a, const int __la, poly16x8_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ poly64x2_t vdupq_n_p64(poly64_t __x) {
  poly64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ poly64x2_t vmovq_n_p64(poly64_t __x) {
  poly64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __x;
  return __r;
}
static __inline__ poly64_t vgetq_lane_p64(poly64x2_t __v, const int __lane) {
  return __v[__lane];
}
static __inline__ poly64x2_t vsetq_lane_p64(poly64_t __x, poly64x2_t __v, const int __lane) {
  __v[__lane] = __x;
  return __v;
}
static __inline__ poly64x2_t vdupq_lane_p64(poly64x1_t __v, const int __lane) {
  poly64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ poly64x2_t vdupq_laneq_p64(poly64x2_t __v, const int __lane) {
  poly64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __v[__lane];
  return __r;
}
static __inline__ poly64x2_t vcopyq_lane_p64(poly64x2_t __a, const int __la, poly64x1_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ poly64x2_t vcopyq_laneq_p64(poly64x2_t __a, const int __la, poly64x2_t __b,
    const int __lb) {
  __a[__la] = __b[__lb];
  return __a;
}
static __inline__ int8x8_t vget_low_s8(int8x16_t __v) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __v[__i];
  return __r;
}
static __inline__ int8x8_t vget_high_s8(int8x16_t __v) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __v[__i + 8];
  return __r;
}
static __inline__ int8x16_t vcombine_s8(int8x8_t __lo, int8x8_t __hi) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __i < 8 ? __lo[__i] : __hi[__i - 8];
  return __r;
}
static __inline__ int8x8_t vcreate_s8(uint64_t __x) {
  int8x8_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ int16x4_t vget_low_s16(int16x8_t __v) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __v[__i];
  return __r;
}
static __inline__ int16x4_t vget_high_s16(int16x8_t __v) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __v[__i + 4];
  return __r;
}
static __inline__ int16x8_t vcombine_s16(int16x4_t __lo, int16x4_t __hi) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i < 4 ? __lo[__i] : __hi[__i - 4];
  return __r;
}
static __inline__ int16x4_t vcreate_s16(uint64_t __x) {
  int16x4_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ int32x2_t vget_low_s32(int32x4_t __v) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __v[__i];
  return __r;
}
static __inline__ int32x2_t vget_high_s32(int32x4_t __v) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __v[__i + 2];
  return __r;
}
static __inline__ int32x4_t vcombine_s32(int32x2_t __lo, int32x2_t __hi) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i < 2 ? __lo[__i] : __hi[__i - 2];
  return __r;
}
static __inline__ int32x2_t vcreate_s32(uint64_t __x) {
  int32x2_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ int64x1_t vget_low_s64(int64x2_t __v) {
  int64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __v[__i];
  return __r;
}
static __inline__ int64x1_t vget_high_s64(int64x2_t __v) {
  int64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __v[__i + 1];
  return __r;
}
static __inline__ int64x2_t vcombine_s64(int64x1_t __lo, int64x1_t __hi) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i < 1 ? __lo[__i] : __hi[__i - 1];
  return __r;
}
static __inline__ int64x1_t vcreate_s64(uint64_t __x) {
  int64x1_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ uint8x8_t vget_low_u8(uint8x16_t __v) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __v[__i];
  return __r;
}
static __inline__ uint8x8_t vget_high_u8(uint8x16_t __v) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __v[__i + 8];
  return __r;
}
static __inline__ uint8x16_t vcombine_u8(uint8x8_t __lo, uint8x8_t __hi) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __i < 8 ? __lo[__i] : __hi[__i - 8];
  return __r;
}
static __inline__ uint8x8_t vcreate_u8(uint64_t __x) {
  uint8x8_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ uint16x4_t vget_low_u16(uint16x8_t __v) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __v[__i];
  return __r;
}
static __inline__ uint16x4_t vget_high_u16(uint16x8_t __v) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __v[__i + 4];
  return __r;
}
static __inline__ uint16x8_t vcombine_u16(uint16x4_t __lo, uint16x4_t __hi) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i < 4 ? __lo[__i] : __hi[__i - 4];
  return __r;
}
static __inline__ uint16x4_t vcreate_u16(uint64_t __x) {
  uint16x4_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ uint32x2_t vget_low_u32(uint32x4_t __v) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __v[__i];
  return __r;
}
static __inline__ uint32x2_t vget_high_u32(uint32x4_t __v) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __v[__i + 2];
  return __r;
}
static __inline__ uint32x4_t vcombine_u32(uint32x2_t __lo, uint32x2_t __hi) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i < 2 ? __lo[__i] : __hi[__i - 2];
  return __r;
}
static __inline__ uint32x2_t vcreate_u32(uint64_t __x) {
  uint32x2_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ uint64x1_t vget_low_u64(uint64x2_t __v) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __v[__i];
  return __r;
}
static __inline__ uint64x1_t vget_high_u64(uint64x2_t __v) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __v[__i + 1];
  return __r;
}
static __inline__ uint64x2_t vcombine_u64(uint64x1_t __lo, uint64x1_t __hi) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i < 1 ? __lo[__i] : __hi[__i - 1];
  return __r;
}
static __inline__ uint64x1_t vcreate_u64(uint64_t __x) {
  uint64x1_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ float32x2_t vget_low_f32(float32x4_t __v) {
  float32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __v[__i];
  return __r;
}
static __inline__ float32x2_t vget_high_f32(float32x4_t __v) {
  float32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __v[__i + 2];
  return __r;
}
static __inline__ float32x4_t vcombine_f32(float32x2_t __lo, float32x2_t __hi) {
  float32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i < 2 ? __lo[__i] : __hi[__i - 2];
  return __r;
}
static __inline__ float32x2_t vcreate_f32(uint64_t __x) {
  float32x2_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ float64x1_t vget_low_f64(float64x2_t __v) {
  float64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __v[__i];
  return __r;
}
static __inline__ float64x1_t vget_high_f64(float64x2_t __v) {
  float64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __v[__i + 1];
  return __r;
}
static __inline__ float64x2_t vcombine_f64(float64x1_t __lo, float64x1_t __hi) {
  float64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i < 1 ? __lo[__i] : __hi[__i - 1];
  return __r;
}
static __inline__ float64x1_t vcreate_f64(uint64_t __x) {
  float64x1_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ poly8x8_t vget_low_p8(poly8x16_t __v) {
  poly8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __v[__i];
  return __r;
}
static __inline__ poly8x8_t vget_high_p8(poly8x16_t __v) {
  poly8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __v[__i + 8];
  return __r;
}
static __inline__ poly8x16_t vcombine_p8(poly8x8_t __lo, poly8x8_t __hi) {
  poly8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __i < 8 ? __lo[__i] : __hi[__i - 8];
  return __r;
}
static __inline__ poly8x8_t vcreate_p8(uint64_t __x) {
  poly8x8_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ poly16x4_t vget_low_p16(poly16x8_t __v) {
  poly16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __v[__i];
  return __r;
}
static __inline__ poly16x4_t vget_high_p16(poly16x8_t __v) {
  poly16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __v[__i + 4];
  return __r;
}
static __inline__ poly16x8_t vcombine_p16(poly16x4_t __lo, poly16x4_t __hi) {
  poly16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i < 4 ? __lo[__i] : __hi[__i - 4];
  return __r;
}
static __inline__ poly16x4_t vcreate_p16(uint64_t __x) {
  poly16x4_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ poly64x1_t vget_low_p64(poly64x2_t __v) {
  poly64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __v[__i];
  return __r;
}
static __inline__ poly64x1_t vget_high_p64(poly64x2_t __v) {
  poly64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __v[__i + 1];
  return __r;
}
static __inline__ poly64x2_t vcombine_p64(poly64x1_t __lo, poly64x1_t __hi) {
  poly64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i < 1 ? __lo[__i] : __hi[__i - 1];
  return __r;
}
static __inline__ poly64x1_t vcreate_p64(uint64_t __x) {
  poly64x1_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}

/* Reinterpretation, which keeps the bytes and changes what the lanes are. */
static __inline__ int8x8_t vreinterpret_s8_s16(int16x4_t __v) {
  int8x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int8x8_t vreinterpret_s8_s32(int32x2_t __v) {
  int8x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int8x8_t vreinterpret_s8_s64(int64x1_t __v) {
  int8x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int8x8_t vreinterpret_s8_u8(uint8x8_t __v) {
  int8x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int8x8_t vreinterpret_s8_u16(uint16x4_t __v) {
  int8x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int8x8_t vreinterpret_s8_u32(uint32x2_t __v) {
  int8x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int8x8_t vreinterpret_s8_u64(uint64x1_t __v) {
  int8x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int8x8_t vreinterpret_s8_f32(float32x2_t __v) {
  int8x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int8x8_t vreinterpret_s8_f64(float64x1_t __v) {
  int8x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int8x8_t vreinterpret_s8_p8(poly8x8_t __v) {
  int8x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int8x8_t vreinterpret_s8_p16(poly16x4_t __v) {
  int8x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int8x8_t vreinterpret_s8_p64(poly64x1_t __v) {
  int8x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int16x4_t vreinterpret_s16_s8(int8x8_t __v) {
  int16x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int16x4_t vreinterpret_s16_s32(int32x2_t __v) {
  int16x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int16x4_t vreinterpret_s16_s64(int64x1_t __v) {
  int16x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int16x4_t vreinterpret_s16_u8(uint8x8_t __v) {
  int16x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int16x4_t vreinterpret_s16_u16(uint16x4_t __v) {
  int16x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int16x4_t vreinterpret_s16_u32(uint32x2_t __v) {
  int16x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int16x4_t vreinterpret_s16_u64(uint64x1_t __v) {
  int16x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int16x4_t vreinterpret_s16_f32(float32x2_t __v) {
  int16x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int16x4_t vreinterpret_s16_f64(float64x1_t __v) {
  int16x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int16x4_t vreinterpret_s16_p8(poly8x8_t __v) {
  int16x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int16x4_t vreinterpret_s16_p16(poly16x4_t __v) {
  int16x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int16x4_t vreinterpret_s16_p64(poly64x1_t __v) {
  int16x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int32x2_t vreinterpret_s32_s8(int8x8_t __v) {
  int32x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int32x2_t vreinterpret_s32_s16(int16x4_t __v) {
  int32x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int32x2_t vreinterpret_s32_s64(int64x1_t __v) {
  int32x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int32x2_t vreinterpret_s32_u8(uint8x8_t __v) {
  int32x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int32x2_t vreinterpret_s32_u16(uint16x4_t __v) {
  int32x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int32x2_t vreinterpret_s32_u32(uint32x2_t __v) {
  int32x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int32x2_t vreinterpret_s32_u64(uint64x1_t __v) {
  int32x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int32x2_t vreinterpret_s32_f32(float32x2_t __v) {
  int32x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int32x2_t vreinterpret_s32_f64(float64x1_t __v) {
  int32x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int32x2_t vreinterpret_s32_p8(poly8x8_t __v) {
  int32x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int32x2_t vreinterpret_s32_p16(poly16x4_t __v) {
  int32x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int32x2_t vreinterpret_s32_p64(poly64x1_t __v) {
  int32x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int64x1_t vreinterpret_s64_s8(int8x8_t __v) {
  int64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int64x1_t vreinterpret_s64_s16(int16x4_t __v) {
  int64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int64x1_t vreinterpret_s64_s32(int32x2_t __v) {
  int64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int64x1_t vreinterpret_s64_u8(uint8x8_t __v) {
  int64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int64x1_t vreinterpret_s64_u16(uint16x4_t __v) {
  int64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int64x1_t vreinterpret_s64_u32(uint32x2_t __v) {
  int64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int64x1_t vreinterpret_s64_u64(uint64x1_t __v) {
  int64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int64x1_t vreinterpret_s64_f32(float32x2_t __v) {
  int64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int64x1_t vreinterpret_s64_f64(float64x1_t __v) {
  int64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int64x1_t vreinterpret_s64_p8(poly8x8_t __v) {
  int64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int64x1_t vreinterpret_s64_p16(poly16x4_t __v) {
  int64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int64x1_t vreinterpret_s64_p64(poly64x1_t __v) {
  int64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint8x8_t vreinterpret_u8_s8(int8x8_t __v) {
  uint8x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint8x8_t vreinterpret_u8_s16(int16x4_t __v) {
  uint8x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint8x8_t vreinterpret_u8_s32(int32x2_t __v) {
  uint8x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint8x8_t vreinterpret_u8_s64(int64x1_t __v) {
  uint8x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint8x8_t vreinterpret_u8_u16(uint16x4_t __v) {
  uint8x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint8x8_t vreinterpret_u8_u32(uint32x2_t __v) {
  uint8x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint8x8_t vreinterpret_u8_u64(uint64x1_t __v) {
  uint8x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint8x8_t vreinterpret_u8_f32(float32x2_t __v) {
  uint8x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint8x8_t vreinterpret_u8_f64(float64x1_t __v) {
  uint8x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint8x8_t vreinterpret_u8_p8(poly8x8_t __v) {
  uint8x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint8x8_t vreinterpret_u8_p16(poly16x4_t __v) {
  uint8x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint8x8_t vreinterpret_u8_p64(poly64x1_t __v) {
  uint8x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint16x4_t vreinterpret_u16_s8(int8x8_t __v) {
  uint16x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint16x4_t vreinterpret_u16_s16(int16x4_t __v) {
  uint16x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint16x4_t vreinterpret_u16_s32(int32x2_t __v) {
  uint16x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint16x4_t vreinterpret_u16_s64(int64x1_t __v) {
  uint16x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint16x4_t vreinterpret_u16_u8(uint8x8_t __v) {
  uint16x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint16x4_t vreinterpret_u16_u32(uint32x2_t __v) {
  uint16x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint16x4_t vreinterpret_u16_u64(uint64x1_t __v) {
  uint16x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint16x4_t vreinterpret_u16_f32(float32x2_t __v) {
  uint16x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint16x4_t vreinterpret_u16_f64(float64x1_t __v) {
  uint16x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint16x4_t vreinterpret_u16_p8(poly8x8_t __v) {
  uint16x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint16x4_t vreinterpret_u16_p16(poly16x4_t __v) {
  uint16x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint16x4_t vreinterpret_u16_p64(poly64x1_t __v) {
  uint16x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint32x2_t vreinterpret_u32_s8(int8x8_t __v) {
  uint32x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint32x2_t vreinterpret_u32_s16(int16x4_t __v) {
  uint32x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint32x2_t vreinterpret_u32_s32(int32x2_t __v) {
  uint32x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint32x2_t vreinterpret_u32_s64(int64x1_t __v) {
  uint32x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint32x2_t vreinterpret_u32_u8(uint8x8_t __v) {
  uint32x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint32x2_t vreinterpret_u32_u16(uint16x4_t __v) {
  uint32x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint32x2_t vreinterpret_u32_u64(uint64x1_t __v) {
  uint32x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint32x2_t vreinterpret_u32_f32(float32x2_t __v) {
  uint32x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint32x2_t vreinterpret_u32_f64(float64x1_t __v) {
  uint32x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint32x2_t vreinterpret_u32_p8(poly8x8_t __v) {
  uint32x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint32x2_t vreinterpret_u32_p16(poly16x4_t __v) {
  uint32x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint32x2_t vreinterpret_u32_p64(poly64x1_t __v) {
  uint32x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint64x1_t vreinterpret_u64_s8(int8x8_t __v) {
  uint64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint64x1_t vreinterpret_u64_s16(int16x4_t __v) {
  uint64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint64x1_t vreinterpret_u64_s32(int32x2_t __v) {
  uint64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint64x1_t vreinterpret_u64_s64(int64x1_t __v) {
  uint64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint64x1_t vreinterpret_u64_u8(uint8x8_t __v) {
  uint64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint64x1_t vreinterpret_u64_u16(uint16x4_t __v) {
  uint64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint64x1_t vreinterpret_u64_u32(uint32x2_t __v) {
  uint64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint64x1_t vreinterpret_u64_f32(float32x2_t __v) {
  uint64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint64x1_t vreinterpret_u64_f64(float64x1_t __v) {
  uint64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint64x1_t vreinterpret_u64_p8(poly8x8_t __v) {
  uint64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint64x1_t vreinterpret_u64_p16(poly16x4_t __v) {
  uint64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint64x1_t vreinterpret_u64_p64(poly64x1_t __v) {
  uint64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float32x2_t vreinterpret_f32_s8(int8x8_t __v) {
  float32x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float32x2_t vreinterpret_f32_s16(int16x4_t __v) {
  float32x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float32x2_t vreinterpret_f32_s32(int32x2_t __v) {
  float32x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float32x2_t vreinterpret_f32_s64(int64x1_t __v) {
  float32x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float32x2_t vreinterpret_f32_u8(uint8x8_t __v) {
  float32x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float32x2_t vreinterpret_f32_u16(uint16x4_t __v) {
  float32x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float32x2_t vreinterpret_f32_u32(uint32x2_t __v) {
  float32x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float32x2_t vreinterpret_f32_u64(uint64x1_t __v) {
  float32x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float32x2_t vreinterpret_f32_f64(float64x1_t __v) {
  float32x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float32x2_t vreinterpret_f32_p8(poly8x8_t __v) {
  float32x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float32x2_t vreinterpret_f32_p16(poly16x4_t __v) {
  float32x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float32x2_t vreinterpret_f32_p64(poly64x1_t __v) {
  float32x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float64x1_t vreinterpret_f64_s8(int8x8_t __v) {
  float64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float64x1_t vreinterpret_f64_s16(int16x4_t __v) {
  float64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float64x1_t vreinterpret_f64_s32(int32x2_t __v) {
  float64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float64x1_t vreinterpret_f64_s64(int64x1_t __v) {
  float64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float64x1_t vreinterpret_f64_u8(uint8x8_t __v) {
  float64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float64x1_t vreinterpret_f64_u16(uint16x4_t __v) {
  float64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float64x1_t vreinterpret_f64_u32(uint32x2_t __v) {
  float64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float64x1_t vreinterpret_f64_u64(uint64x1_t __v) {
  float64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float64x1_t vreinterpret_f64_f32(float32x2_t __v) {
  float64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float64x1_t vreinterpret_f64_p8(poly8x8_t __v) {
  float64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float64x1_t vreinterpret_f64_p16(poly16x4_t __v) {
  float64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float64x1_t vreinterpret_f64_p64(poly64x1_t __v) {
  float64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly8x8_t vreinterpret_p8_s8(int8x8_t __v) {
  poly8x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly8x8_t vreinterpret_p8_s16(int16x4_t __v) {
  poly8x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly8x8_t vreinterpret_p8_s32(int32x2_t __v) {
  poly8x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly8x8_t vreinterpret_p8_s64(int64x1_t __v) {
  poly8x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly8x8_t vreinterpret_p8_u8(uint8x8_t __v) {
  poly8x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly8x8_t vreinterpret_p8_u16(uint16x4_t __v) {
  poly8x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly8x8_t vreinterpret_p8_u32(uint32x2_t __v) {
  poly8x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly8x8_t vreinterpret_p8_u64(uint64x1_t __v) {
  poly8x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly8x8_t vreinterpret_p8_f32(float32x2_t __v) {
  poly8x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly8x8_t vreinterpret_p8_f64(float64x1_t __v) {
  poly8x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly8x8_t vreinterpret_p8_p16(poly16x4_t __v) {
  poly8x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly8x8_t vreinterpret_p8_p64(poly64x1_t __v) {
  poly8x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly16x4_t vreinterpret_p16_s8(int8x8_t __v) {
  poly16x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly16x4_t vreinterpret_p16_s16(int16x4_t __v) {
  poly16x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly16x4_t vreinterpret_p16_s32(int32x2_t __v) {
  poly16x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly16x4_t vreinterpret_p16_s64(int64x1_t __v) {
  poly16x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly16x4_t vreinterpret_p16_u8(uint8x8_t __v) {
  poly16x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly16x4_t vreinterpret_p16_u16(uint16x4_t __v) {
  poly16x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly16x4_t vreinterpret_p16_u32(uint32x2_t __v) {
  poly16x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly16x4_t vreinterpret_p16_u64(uint64x1_t __v) {
  poly16x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly16x4_t vreinterpret_p16_f32(float32x2_t __v) {
  poly16x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly16x4_t vreinterpret_p16_f64(float64x1_t __v) {
  poly16x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly16x4_t vreinterpret_p16_p8(poly8x8_t __v) {
  poly16x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly16x4_t vreinterpret_p16_p64(poly64x1_t __v) {
  poly16x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly64x1_t vreinterpret_p64_s8(int8x8_t __v) {
  poly64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly64x1_t vreinterpret_p64_s16(int16x4_t __v) {
  poly64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly64x1_t vreinterpret_p64_s32(int32x2_t __v) {
  poly64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly64x1_t vreinterpret_p64_s64(int64x1_t __v) {
  poly64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly64x1_t vreinterpret_p64_u8(uint8x8_t __v) {
  poly64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly64x1_t vreinterpret_p64_u16(uint16x4_t __v) {
  poly64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly64x1_t vreinterpret_p64_u32(uint32x2_t __v) {
  poly64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly64x1_t vreinterpret_p64_u64(uint64x1_t __v) {
  poly64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly64x1_t vreinterpret_p64_f32(float32x2_t __v) {
  poly64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly64x1_t vreinterpret_p64_f64(float64x1_t __v) {
  poly64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly64x1_t vreinterpret_p64_p8(poly8x8_t __v) {
  poly64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly64x1_t vreinterpret_p64_p16(poly16x4_t __v) {
  poly64x1_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int8x16_t vreinterpretq_s8_s16(int16x8_t __v) {
  int8x16_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int8x16_t vreinterpretq_s8_s32(int32x4_t __v) {
  int8x16_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int8x16_t vreinterpretq_s8_s64(int64x2_t __v) {
  int8x16_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int8x16_t vreinterpretq_s8_u8(uint8x16_t __v) {
  int8x16_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int8x16_t vreinterpretq_s8_u16(uint16x8_t __v) {
  int8x16_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int8x16_t vreinterpretq_s8_u32(uint32x4_t __v) {
  int8x16_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int8x16_t vreinterpretq_s8_u64(uint64x2_t __v) {
  int8x16_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int8x16_t vreinterpretq_s8_f32(float32x4_t __v) {
  int8x16_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int8x16_t vreinterpretq_s8_f64(float64x2_t __v) {
  int8x16_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int8x16_t vreinterpretq_s8_p8(poly8x16_t __v) {
  int8x16_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int8x16_t vreinterpretq_s8_p16(poly16x8_t __v) {
  int8x16_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int8x16_t vreinterpretq_s8_p64(poly64x2_t __v) {
  int8x16_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int16x8_t vreinterpretq_s16_s8(int8x16_t __v) {
  int16x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int16x8_t vreinterpretq_s16_s32(int32x4_t __v) {
  int16x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int16x8_t vreinterpretq_s16_s64(int64x2_t __v) {
  int16x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int16x8_t vreinterpretq_s16_u8(uint8x16_t __v) {
  int16x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int16x8_t vreinterpretq_s16_u16(uint16x8_t __v) {
  int16x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int16x8_t vreinterpretq_s16_u32(uint32x4_t __v) {
  int16x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int16x8_t vreinterpretq_s16_u64(uint64x2_t __v) {
  int16x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int16x8_t vreinterpretq_s16_f32(float32x4_t __v) {
  int16x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int16x8_t vreinterpretq_s16_f64(float64x2_t __v) {
  int16x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int16x8_t vreinterpretq_s16_p8(poly8x16_t __v) {
  int16x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int16x8_t vreinterpretq_s16_p16(poly16x8_t __v) {
  int16x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int16x8_t vreinterpretq_s16_p64(poly64x2_t __v) {
  int16x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int32x4_t vreinterpretq_s32_s8(int8x16_t __v) {
  int32x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int32x4_t vreinterpretq_s32_s16(int16x8_t __v) {
  int32x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int32x4_t vreinterpretq_s32_s64(int64x2_t __v) {
  int32x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int32x4_t vreinterpretq_s32_u8(uint8x16_t __v) {
  int32x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int32x4_t vreinterpretq_s32_u16(uint16x8_t __v) {
  int32x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int32x4_t vreinterpretq_s32_u32(uint32x4_t __v) {
  int32x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int32x4_t vreinterpretq_s32_u64(uint64x2_t __v) {
  int32x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int32x4_t vreinterpretq_s32_f32(float32x4_t __v) {
  int32x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int32x4_t vreinterpretq_s32_f64(float64x2_t __v) {
  int32x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int32x4_t vreinterpretq_s32_p8(poly8x16_t __v) {
  int32x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int32x4_t vreinterpretq_s32_p16(poly16x8_t __v) {
  int32x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int32x4_t vreinterpretq_s32_p64(poly64x2_t __v) {
  int32x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int64x2_t vreinterpretq_s64_s8(int8x16_t __v) {
  int64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int64x2_t vreinterpretq_s64_s16(int16x8_t __v) {
  int64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int64x2_t vreinterpretq_s64_s32(int32x4_t __v) {
  int64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int64x2_t vreinterpretq_s64_u8(uint8x16_t __v) {
  int64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int64x2_t vreinterpretq_s64_u16(uint16x8_t __v) {
  int64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int64x2_t vreinterpretq_s64_u32(uint32x4_t __v) {
  int64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int64x2_t vreinterpretq_s64_u64(uint64x2_t __v) {
  int64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int64x2_t vreinterpretq_s64_f32(float32x4_t __v) {
  int64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int64x2_t vreinterpretq_s64_f64(float64x2_t __v) {
  int64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int64x2_t vreinterpretq_s64_p8(poly8x16_t __v) {
  int64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int64x2_t vreinterpretq_s64_p16(poly16x8_t __v) {
  int64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ int64x2_t vreinterpretq_s64_p64(poly64x2_t __v) {
  int64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint8x16_t vreinterpretq_u8_s8(int8x16_t __v) {
  uint8x16_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint8x16_t vreinterpretq_u8_s16(int16x8_t __v) {
  uint8x16_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint8x16_t vreinterpretq_u8_s32(int32x4_t __v) {
  uint8x16_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint8x16_t vreinterpretq_u8_s64(int64x2_t __v) {
  uint8x16_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint8x16_t vreinterpretq_u8_u16(uint16x8_t __v) {
  uint8x16_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint8x16_t vreinterpretq_u8_u32(uint32x4_t __v) {
  uint8x16_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint8x16_t vreinterpretq_u8_u64(uint64x2_t __v) {
  uint8x16_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint8x16_t vreinterpretq_u8_f32(float32x4_t __v) {
  uint8x16_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint8x16_t vreinterpretq_u8_f64(float64x2_t __v) {
  uint8x16_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint8x16_t vreinterpretq_u8_p8(poly8x16_t __v) {
  uint8x16_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint8x16_t vreinterpretq_u8_p16(poly16x8_t __v) {
  uint8x16_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint8x16_t vreinterpretq_u8_p64(poly64x2_t __v) {
  uint8x16_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint16x8_t vreinterpretq_u16_s8(int8x16_t __v) {
  uint16x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint16x8_t vreinterpretq_u16_s16(int16x8_t __v) {
  uint16x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint16x8_t vreinterpretq_u16_s32(int32x4_t __v) {
  uint16x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint16x8_t vreinterpretq_u16_s64(int64x2_t __v) {
  uint16x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint16x8_t vreinterpretq_u16_u8(uint8x16_t __v) {
  uint16x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint16x8_t vreinterpretq_u16_u32(uint32x4_t __v) {
  uint16x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint16x8_t vreinterpretq_u16_u64(uint64x2_t __v) {
  uint16x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint16x8_t vreinterpretq_u16_f32(float32x4_t __v) {
  uint16x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint16x8_t vreinterpretq_u16_f64(float64x2_t __v) {
  uint16x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint16x8_t vreinterpretq_u16_p8(poly8x16_t __v) {
  uint16x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint16x8_t vreinterpretq_u16_p16(poly16x8_t __v) {
  uint16x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint16x8_t vreinterpretq_u16_p64(poly64x2_t __v) {
  uint16x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint32x4_t vreinterpretq_u32_s8(int8x16_t __v) {
  uint32x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint32x4_t vreinterpretq_u32_s16(int16x8_t __v) {
  uint32x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint32x4_t vreinterpretq_u32_s32(int32x4_t __v) {
  uint32x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint32x4_t vreinterpretq_u32_s64(int64x2_t __v) {
  uint32x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint32x4_t vreinterpretq_u32_u8(uint8x16_t __v) {
  uint32x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint32x4_t vreinterpretq_u32_u16(uint16x8_t __v) {
  uint32x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint32x4_t vreinterpretq_u32_u64(uint64x2_t __v) {
  uint32x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint32x4_t vreinterpretq_u32_f32(float32x4_t __v) {
  uint32x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint32x4_t vreinterpretq_u32_f64(float64x2_t __v) {
  uint32x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint32x4_t vreinterpretq_u32_p8(poly8x16_t __v) {
  uint32x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint32x4_t vreinterpretq_u32_p16(poly16x8_t __v) {
  uint32x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint32x4_t vreinterpretq_u32_p64(poly64x2_t __v) {
  uint32x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint64x2_t vreinterpretq_u64_s8(int8x16_t __v) {
  uint64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint64x2_t vreinterpretq_u64_s16(int16x8_t __v) {
  uint64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint64x2_t vreinterpretq_u64_s32(int32x4_t __v) {
  uint64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint64x2_t vreinterpretq_u64_s64(int64x2_t __v) {
  uint64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint64x2_t vreinterpretq_u64_u8(uint8x16_t __v) {
  uint64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint64x2_t vreinterpretq_u64_u16(uint16x8_t __v) {
  uint64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint64x2_t vreinterpretq_u64_u32(uint32x4_t __v) {
  uint64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint64x2_t vreinterpretq_u64_f32(float32x4_t __v) {
  uint64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint64x2_t vreinterpretq_u64_f64(float64x2_t __v) {
  uint64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint64x2_t vreinterpretq_u64_p8(poly8x16_t __v) {
  uint64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint64x2_t vreinterpretq_u64_p16(poly16x8_t __v) {
  uint64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ uint64x2_t vreinterpretq_u64_p64(poly64x2_t __v) {
  uint64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float32x4_t vreinterpretq_f32_s8(int8x16_t __v) {
  float32x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float32x4_t vreinterpretq_f32_s16(int16x8_t __v) {
  float32x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float32x4_t vreinterpretq_f32_s32(int32x4_t __v) {
  float32x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float32x4_t vreinterpretq_f32_s64(int64x2_t __v) {
  float32x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float32x4_t vreinterpretq_f32_u8(uint8x16_t __v) {
  float32x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float32x4_t vreinterpretq_f32_u16(uint16x8_t __v) {
  float32x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float32x4_t vreinterpretq_f32_u32(uint32x4_t __v) {
  float32x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float32x4_t vreinterpretq_f32_u64(uint64x2_t __v) {
  float32x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float32x4_t vreinterpretq_f32_f64(float64x2_t __v) {
  float32x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float32x4_t vreinterpretq_f32_p8(poly8x16_t __v) {
  float32x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float32x4_t vreinterpretq_f32_p16(poly16x8_t __v) {
  float32x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float32x4_t vreinterpretq_f32_p64(poly64x2_t __v) {
  float32x4_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float64x2_t vreinterpretq_f64_s8(int8x16_t __v) {
  float64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float64x2_t vreinterpretq_f64_s16(int16x8_t __v) {
  float64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float64x2_t vreinterpretq_f64_s32(int32x4_t __v) {
  float64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float64x2_t vreinterpretq_f64_s64(int64x2_t __v) {
  float64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float64x2_t vreinterpretq_f64_u8(uint8x16_t __v) {
  float64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float64x2_t vreinterpretq_f64_u16(uint16x8_t __v) {
  float64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float64x2_t vreinterpretq_f64_u32(uint32x4_t __v) {
  float64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float64x2_t vreinterpretq_f64_u64(uint64x2_t __v) {
  float64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float64x2_t vreinterpretq_f64_f32(float32x4_t __v) {
  float64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float64x2_t vreinterpretq_f64_p8(poly8x16_t __v) {
  float64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float64x2_t vreinterpretq_f64_p16(poly16x8_t __v) {
  float64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ float64x2_t vreinterpretq_f64_p64(poly64x2_t __v) {
  float64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly8x16_t vreinterpretq_p8_s8(int8x16_t __v) {
  poly8x16_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly8x16_t vreinterpretq_p8_s16(int16x8_t __v) {
  poly8x16_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly8x16_t vreinterpretq_p8_s32(int32x4_t __v) {
  poly8x16_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly8x16_t vreinterpretq_p8_s64(int64x2_t __v) {
  poly8x16_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly8x16_t vreinterpretq_p8_u8(uint8x16_t __v) {
  poly8x16_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly8x16_t vreinterpretq_p8_u16(uint16x8_t __v) {
  poly8x16_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly8x16_t vreinterpretq_p8_u32(uint32x4_t __v) {
  poly8x16_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly8x16_t vreinterpretq_p8_u64(uint64x2_t __v) {
  poly8x16_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly8x16_t vreinterpretq_p8_f32(float32x4_t __v) {
  poly8x16_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly8x16_t vreinterpretq_p8_f64(float64x2_t __v) {
  poly8x16_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly8x16_t vreinterpretq_p8_p16(poly16x8_t __v) {
  poly8x16_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly8x16_t vreinterpretq_p8_p64(poly64x2_t __v) {
  poly8x16_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly16x8_t vreinterpretq_p16_s8(int8x16_t __v) {
  poly16x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly16x8_t vreinterpretq_p16_s16(int16x8_t __v) {
  poly16x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly16x8_t vreinterpretq_p16_s32(int32x4_t __v) {
  poly16x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly16x8_t vreinterpretq_p16_s64(int64x2_t __v) {
  poly16x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly16x8_t vreinterpretq_p16_u8(uint8x16_t __v) {
  poly16x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly16x8_t vreinterpretq_p16_u16(uint16x8_t __v) {
  poly16x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly16x8_t vreinterpretq_p16_u32(uint32x4_t __v) {
  poly16x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly16x8_t vreinterpretq_p16_u64(uint64x2_t __v) {
  poly16x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly16x8_t vreinterpretq_p16_f32(float32x4_t __v) {
  poly16x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly16x8_t vreinterpretq_p16_f64(float64x2_t __v) {
  poly16x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly16x8_t vreinterpretq_p16_p8(poly8x16_t __v) {
  poly16x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly16x8_t vreinterpretq_p16_p64(poly64x2_t __v) {
  poly16x8_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly64x2_t vreinterpretq_p64_s8(int8x16_t __v) {
  poly64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly64x2_t vreinterpretq_p64_s16(int16x8_t __v) {
  poly64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly64x2_t vreinterpretq_p64_s32(int32x4_t __v) {
  poly64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly64x2_t vreinterpretq_p64_s64(int64x2_t __v) {
  poly64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly64x2_t vreinterpretq_p64_u8(uint8x16_t __v) {
  poly64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly64x2_t vreinterpretq_p64_u16(uint16x8_t __v) {
  poly64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly64x2_t vreinterpretq_p64_u32(uint32x4_t __v) {
  poly64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly64x2_t vreinterpretq_p64_u64(uint64x2_t __v) {
  poly64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly64x2_t vreinterpretq_p64_f32(float32x4_t __v) {
  poly64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly64x2_t vreinterpretq_p64_f64(float64x2_t __v) {
  poly64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly64x2_t vreinterpretq_p64_p8(poly8x16_t __v) {
  poly64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}
static __inline__ poly64x2_t vreinterpretq_p64_p16(poly16x8_t __v) {
  poly64x2_t __r;
  __builtin_memcpy(&__r, &__v, sizeof __r);
  return __r;
}

/* Arithmetic and logic, a lane at a time. */
static __inline__ int8x8_t vadd_s8(int8x8_t __a, int8x8_t __b) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int8_t)(__a[__i] + __b[__i]);
  return __r;
}
static __inline__ int8x8_t vsub_s8(int8x8_t __a, int8x8_t __b) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int8_t)(__a[__i] - __b[__i]);
  return __r;
}
static __inline__ int8x8_t vmul_s8(int8x8_t __a, int8x8_t __b) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int8_t)(__a[__i] * __b[__i]);
  return __r;
}
static __inline__ int8x8_t vmla_s8(int8x8_t __a, int8x8_t __b, int8x8_t __c) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int8_t)(__a[__i] + __b[__i] * __c[__i]);
  return __r;
}
static __inline__ int8x8_t vmls_s8(int8x8_t __a, int8x8_t __b, int8x8_t __c) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int8_t)(__a[__i] - __b[__i] * __c[__i]);
  return __r;
}
static __inline__ int8x8_t vmax_s8(int8x8_t __a, int8x8_t __b) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] > __b[__i] ? __a[__i] : __b[__i];
  return __r;
}
static __inline__ int8x8_t vmin_s8(int8x8_t __a, int8x8_t __b) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] < __b[__i] ? __a[__i] : __b[__i];
  return __r;
}
static __inline__ int8x8_t vabd_s8(int8x8_t __a, int8x8_t __b) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (int8_t)(__a[__i] > __b[__i] ? __a[__i] - __b[__i] : __b[__i] - __a[__i]);
  return __r;
}
static __inline__ uint8x8_t vceq_s8(int8x8_t __a, int8x8_t __b) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] == __b[__i] ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ uint8x8_t vceqz_s8(int8x8_t __a) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] == 0 ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ uint8x8_t vcge_s8(int8x8_t __a, int8x8_t __b) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] >= __b[__i] ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ uint8x8_t vcgez_s8(int8x8_t __a) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] >= 0 ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ uint8x8_t vcgt_s8(int8x8_t __a, int8x8_t __b) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] > __b[__i] ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ uint8x8_t vcgtz_s8(int8x8_t __a) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] > 0 ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ uint8x8_t vcle_s8(int8x8_t __a, int8x8_t __b) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] <= __b[__i] ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ uint8x8_t vclez_s8(int8x8_t __a) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] <= 0 ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ uint8x8_t vclt_s8(int8x8_t __a, int8x8_t __b) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] < __b[__i] ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ uint8x8_t vcltz_s8(int8x8_t __a) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] < 0 ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ int8_t vaddv_s8(int8x8_t __a) {
  int8_t __r = 0;
  for (int __i = 0; __i < 8; __i++) __r += __a[__i];
  return __r;
}
static __inline__ int8_t vmaxv_s8(int8x8_t __a) {
  int8_t __r = __a[0];
  for (int __i = 1; __i < 8; __i++) if (__a[__i] > __r) __r = __a[__i];
  return __r;
}
static __inline__ int8_t vminv_s8(int8x8_t __a) {
  int8_t __r = __a[0];
  for (int __i = 1; __i < 8; __i++) if (__a[__i] < __r) __r = __a[__i];
  return __r;
}
static __inline__ int8x8_t vpadd_s8(int8x8_t __a, int8x8_t __b) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = __i < 4 ? (int8_t)(__a[2 * __i] + __a[2 * __i + 1]) : (int8_t)(__b[2 * (__i - 4)]
        + __b[2 * (__i - 4) + 1]);
  return __r;
}
static __inline__ int16x4_t vadd_s16(int16x4_t __a, int16x4_t __b) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int16_t)(__a[__i] + __b[__i]);
  return __r;
}
static __inline__ int16x4_t vsub_s16(int16x4_t __a, int16x4_t __b) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int16_t)(__a[__i] - __b[__i]);
  return __r;
}
static __inline__ int16x4_t vmul_s16(int16x4_t __a, int16x4_t __b) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int16_t)(__a[__i] * __b[__i]);
  return __r;
}
static __inline__ int16x4_t vmul_n_s16(int16x4_t __a, int16_t __b) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int16_t)(__a[__i] * __b);
  return __r;
}
static __inline__ int16x4_t vmla_s16(int16x4_t __a, int16x4_t __b, int16x4_t __c) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int16_t)(__a[__i] + __b[__i] * __c[__i]);
  return __r;
}
static __inline__ int16x4_t vmls_s16(int16x4_t __a, int16x4_t __b, int16x4_t __c) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int16_t)(__a[__i] - __b[__i] * __c[__i]);
  return __r;
}
static __inline__ int16x4_t vmla_n_s16(int16x4_t __a, int16x4_t __b, int16_t __c) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int16_t)(__a[__i] + __b[__i] * __c);
  return __r;
}
static __inline__ int16x4_t vmax_s16(int16x4_t __a, int16x4_t __b) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] > __b[__i] ? __a[__i] : __b[__i];
  return __r;
}
static __inline__ int16x4_t vmin_s16(int16x4_t __a, int16x4_t __b) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] < __b[__i] ? __a[__i] : __b[__i];
  return __r;
}
static __inline__ int16x4_t vabd_s16(int16x4_t __a, int16x4_t __b) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (int16_t)(__a[__i] > __b[__i] ? __a[__i] - __b[__i] : __b[__i] - __a[__i]);
  return __r;
}
static __inline__ uint16x4_t vceq_s16(int16x4_t __a, int16x4_t __b) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] == __b[__i] ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ uint16x4_t vceqz_s16(int16x4_t __a) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] == 0 ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ uint16x4_t vcge_s16(int16x4_t __a, int16x4_t __b) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] >= __b[__i] ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ uint16x4_t vcgez_s16(int16x4_t __a) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] >= 0 ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ uint16x4_t vcgt_s16(int16x4_t __a, int16x4_t __b) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] > __b[__i] ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ uint16x4_t vcgtz_s16(int16x4_t __a) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] > 0 ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ uint16x4_t vcle_s16(int16x4_t __a, int16x4_t __b) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] <= __b[__i] ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ uint16x4_t vclez_s16(int16x4_t __a) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] <= 0 ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ uint16x4_t vclt_s16(int16x4_t __a, int16x4_t __b) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] < __b[__i] ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ uint16x4_t vcltz_s16(int16x4_t __a) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] < 0 ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ int16_t vaddv_s16(int16x4_t __a) {
  int16_t __r = 0;
  for (int __i = 0; __i < 4; __i++) __r += __a[__i];
  return __r;
}
static __inline__ int16_t vmaxv_s16(int16x4_t __a) {
  int16_t __r = __a[0];
  for (int __i = 1; __i < 4; __i++) if (__a[__i] > __r) __r = __a[__i];
  return __r;
}
static __inline__ int16_t vminv_s16(int16x4_t __a) {
  int16_t __r = __a[0];
  for (int __i = 1; __i < 4; __i++) if (__a[__i] < __r) __r = __a[__i];
  return __r;
}
static __inline__ int16x4_t vpadd_s16(int16x4_t __a, int16x4_t __b) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = __i < 2 ? (int16_t)(__a[2 * __i] + __a[2 * __i + 1])
        : (int16_t)(__b[2 * (__i - 2)] + __b[2 * (__i - 2) + 1]);
  return __r;
}
static __inline__ int32x2_t vadd_s32(int32x2_t __a, int32x2_t __b) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int32_t)(__a[__i] + __b[__i]);
  return __r;
}
static __inline__ int32x2_t vsub_s32(int32x2_t __a, int32x2_t __b) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int32_t)(__a[__i] - __b[__i]);
  return __r;
}
static __inline__ int32x2_t vmul_s32(int32x2_t __a, int32x2_t __b) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int32_t)(__a[__i] * __b[__i]);
  return __r;
}
static __inline__ int32x2_t vmul_n_s32(int32x2_t __a, int32_t __b) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int32_t)(__a[__i] * __b);
  return __r;
}
static __inline__ int32x2_t vmla_s32(int32x2_t __a, int32x2_t __b, int32x2_t __c) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int32_t)(__a[__i] + __b[__i] * __c[__i]);
  return __r;
}
static __inline__ int32x2_t vmls_s32(int32x2_t __a, int32x2_t __b, int32x2_t __c) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int32_t)(__a[__i] - __b[__i] * __c[__i]);
  return __r;
}
static __inline__ int32x2_t vmla_n_s32(int32x2_t __a, int32x2_t __b, int32_t __c) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int32_t)(__a[__i] + __b[__i] * __c);
  return __r;
}
static __inline__ int32x2_t vmax_s32(int32x2_t __a, int32x2_t __b) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] > __b[__i] ? __a[__i] : __b[__i];
  return __r;
}
static __inline__ int32x2_t vmin_s32(int32x2_t __a, int32x2_t __b) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] < __b[__i] ? __a[__i] : __b[__i];
  return __r;
}
static __inline__ int32x2_t vabd_s32(int32x2_t __a, int32x2_t __b) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (int32_t)(__a[__i] > __b[__i] ? __a[__i] - __b[__i] : __b[__i] - __a[__i]);
  return __r;
}
static __inline__ uint32x2_t vceq_s32(int32x2_t __a, int32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] == __b[__i] ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x2_t vceqz_s32(int32x2_t __a) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] == 0 ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x2_t vcge_s32(int32x2_t __a, int32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] >= __b[__i] ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x2_t vcgez_s32(int32x2_t __a) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] >= 0 ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x2_t vcgt_s32(int32x2_t __a, int32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] > __b[__i] ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x2_t vcgtz_s32(int32x2_t __a) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] > 0 ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x2_t vcle_s32(int32x2_t __a, int32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] <= __b[__i] ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x2_t vclez_s32(int32x2_t __a) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] <= 0 ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x2_t vclt_s32(int32x2_t __a, int32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] < __b[__i] ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x2_t vcltz_s32(int32x2_t __a) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] < 0 ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ int32_t vaddv_s32(int32x2_t __a) {
  int32_t __r = 0;
  for (int __i = 0; __i < 2; __i++) __r += __a[__i];
  return __r;
}
static __inline__ int32_t vmaxv_s32(int32x2_t __a) {
  int32_t __r = __a[0];
  for (int __i = 1; __i < 2; __i++) if (__a[__i] > __r) __r = __a[__i];
  return __r;
}
static __inline__ int32_t vminv_s32(int32x2_t __a) {
  int32_t __r = __a[0];
  for (int __i = 1; __i < 2; __i++) if (__a[__i] < __r) __r = __a[__i];
  return __r;
}
static __inline__ int32x2_t vpadd_s32(int32x2_t __a, int32x2_t __b) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = __i < 1 ? (int32_t)(__a[2 * __i] + __a[2 * __i + 1])
        : (int32_t)(__b[2 * (__i - 1)] + __b[2 * (__i - 1) + 1]);
  return __r;
}
static __inline__ int64x1_t vadd_s64(int64x1_t __a, int64x1_t __b) {
  int64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = (int64_t)(__a[__i] + __b[__i]);
  return __r;
}
static __inline__ int64x1_t vsub_s64(int64x1_t __a, int64x1_t __b) {
  int64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = (int64_t)(__a[__i] - __b[__i]);
  return __r;
}
static __inline__ uint64x1_t vceq_s64(int64x1_t __a, int64x1_t __b) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __a[__i] == __b[__i] ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x1_t vceqz_s64(int64x1_t __a) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __a[__i] == 0 ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x1_t vcge_s64(int64x1_t __a, int64x1_t __b) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __a[__i] >= __b[__i] ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x1_t vcgez_s64(int64x1_t __a) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __a[__i] >= 0 ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x1_t vcgt_s64(int64x1_t __a, int64x1_t __b) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __a[__i] > __b[__i] ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x1_t vcgtz_s64(int64x1_t __a) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __a[__i] > 0 ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x1_t vcle_s64(int64x1_t __a, int64x1_t __b) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __a[__i] <= __b[__i] ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x1_t vclez_s64(int64x1_t __a) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __a[__i] <= 0 ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x1_t vclt_s64(int64x1_t __a, int64x1_t __b) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __a[__i] < __b[__i] ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x1_t vcltz_s64(int64x1_t __a) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __a[__i] < 0 ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint8x8_t vadd_u8(uint8x8_t __a, uint8x8_t __b) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint8_t)(__a[__i] + __b[__i]);
  return __r;
}
static __inline__ uint8x8_t vsub_u8(uint8x8_t __a, uint8x8_t __b) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint8_t)(__a[__i] - __b[__i]);
  return __r;
}
static __inline__ uint8x8_t vmul_u8(uint8x8_t __a, uint8x8_t __b) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint8_t)(__a[__i] * __b[__i]);
  return __r;
}
static __inline__ uint8x8_t vmla_u8(uint8x8_t __a, uint8x8_t __b, uint8x8_t __c) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint8_t)(__a[__i] + __b[__i] * __c[__i]);
  return __r;
}
static __inline__ uint8x8_t vmls_u8(uint8x8_t __a, uint8x8_t __b, uint8x8_t __c) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint8_t)(__a[__i] - __b[__i] * __c[__i]);
  return __r;
}
static __inline__ uint8x8_t vmax_u8(uint8x8_t __a, uint8x8_t __b) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] > __b[__i] ? __a[__i] : __b[__i];
  return __r;
}
static __inline__ uint8x8_t vmin_u8(uint8x8_t __a, uint8x8_t __b) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] < __b[__i] ? __a[__i] : __b[__i];
  return __r;
}
static __inline__ uint8x8_t vabd_u8(uint8x8_t __a, uint8x8_t __b) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (uint8_t)(__a[__i] > __b[__i] ? __a[__i] - __b[__i] : __b[__i] - __a[__i]);
  return __r;
}
static __inline__ uint8x8_t vceq_u8(uint8x8_t __a, uint8x8_t __b) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] == __b[__i] ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ uint8x8_t vceqz_u8(uint8x8_t __a) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] == 0 ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ uint8x8_t vcge_u8(uint8x8_t __a, uint8x8_t __b) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] >= __b[__i] ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ uint8x8_t vcgt_u8(uint8x8_t __a, uint8x8_t __b) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] > __b[__i] ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ uint8x8_t vcle_u8(uint8x8_t __a, uint8x8_t __b) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] <= __b[__i] ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ uint8x8_t vclt_u8(uint8x8_t __a, uint8x8_t __b) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] < __b[__i] ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ uint8_t vaddv_u8(uint8x8_t __a) {
  uint8_t __r = 0;
  for (int __i = 0; __i < 8; __i++) __r += __a[__i];
  return __r;
}
static __inline__ uint8_t vmaxv_u8(uint8x8_t __a) {
  uint8_t __r = __a[0];
  for (int __i = 1; __i < 8; __i++) if (__a[__i] > __r) __r = __a[__i];
  return __r;
}
static __inline__ uint8_t vminv_u8(uint8x8_t __a) {
  uint8_t __r = __a[0];
  for (int __i = 1; __i < 8; __i++) if (__a[__i] < __r) __r = __a[__i];
  return __r;
}
static __inline__ uint8x8_t vpadd_u8(uint8x8_t __a, uint8x8_t __b) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = __i < 4 ? (uint8_t)(__a[2 * __i] + __a[2 * __i + 1])
        : (uint8_t)(__b[2 * (__i - 4)] + __b[2 * (__i - 4) + 1]);
  return __r;
}
static __inline__ uint16x4_t vadd_u16(uint16x4_t __a, uint16x4_t __b) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint16_t)(__a[__i] + __b[__i]);
  return __r;
}
static __inline__ uint16x4_t vsub_u16(uint16x4_t __a, uint16x4_t __b) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint16_t)(__a[__i] - __b[__i]);
  return __r;
}
static __inline__ uint16x4_t vmul_u16(uint16x4_t __a, uint16x4_t __b) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint16_t)(__a[__i] * __b[__i]);
  return __r;
}
static __inline__ uint16x4_t vmul_n_u16(uint16x4_t __a, uint16_t __b) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint16_t)(__a[__i] * __b);
  return __r;
}
static __inline__ uint16x4_t vmla_u16(uint16x4_t __a, uint16x4_t __b, uint16x4_t __c) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint16_t)(__a[__i] + __b[__i] * __c[__i]);
  return __r;
}
static __inline__ uint16x4_t vmls_u16(uint16x4_t __a, uint16x4_t __b, uint16x4_t __c) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint16_t)(__a[__i] - __b[__i] * __c[__i]);
  return __r;
}
static __inline__ uint16x4_t vmla_n_u16(uint16x4_t __a, uint16x4_t __b, uint16_t __c) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint16_t)(__a[__i] + __b[__i] * __c);
  return __r;
}
static __inline__ uint16x4_t vmax_u16(uint16x4_t __a, uint16x4_t __b) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] > __b[__i] ? __a[__i] : __b[__i];
  return __r;
}
static __inline__ uint16x4_t vmin_u16(uint16x4_t __a, uint16x4_t __b) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] < __b[__i] ? __a[__i] : __b[__i];
  return __r;
}
static __inline__ uint16x4_t vabd_u16(uint16x4_t __a, uint16x4_t __b) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (uint16_t)(__a[__i] > __b[__i] ? __a[__i] - __b[__i] : __b[__i] - __a[__i]);
  return __r;
}
static __inline__ uint16x4_t vceq_u16(uint16x4_t __a, uint16x4_t __b) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] == __b[__i] ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ uint16x4_t vceqz_u16(uint16x4_t __a) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] == 0 ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ uint16x4_t vcge_u16(uint16x4_t __a, uint16x4_t __b) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] >= __b[__i] ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ uint16x4_t vcgt_u16(uint16x4_t __a, uint16x4_t __b) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] > __b[__i] ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ uint16x4_t vcle_u16(uint16x4_t __a, uint16x4_t __b) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] <= __b[__i] ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ uint16x4_t vclt_u16(uint16x4_t __a, uint16x4_t __b) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] < __b[__i] ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ uint16_t vaddv_u16(uint16x4_t __a) {
  uint16_t __r = 0;
  for (int __i = 0; __i < 4; __i++) __r += __a[__i];
  return __r;
}
static __inline__ uint16_t vmaxv_u16(uint16x4_t __a) {
  uint16_t __r = __a[0];
  for (int __i = 1; __i < 4; __i++) if (__a[__i] > __r) __r = __a[__i];
  return __r;
}
static __inline__ uint16_t vminv_u16(uint16x4_t __a) {
  uint16_t __r = __a[0];
  for (int __i = 1; __i < 4; __i++) if (__a[__i] < __r) __r = __a[__i];
  return __r;
}
static __inline__ uint16x4_t vpadd_u16(uint16x4_t __a, uint16x4_t __b) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = __i < 2 ? (uint16_t)(__a[2 * __i] + __a[2 * __i + 1])
        : (uint16_t)(__b[2 * (__i - 2)] + __b[2 * (__i - 2) + 1]);
  return __r;
}
static __inline__ uint32x2_t vadd_u32(uint32x2_t __a, uint32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint32_t)(__a[__i] + __b[__i]);
  return __r;
}
static __inline__ uint32x2_t vsub_u32(uint32x2_t __a, uint32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint32_t)(__a[__i] - __b[__i]);
  return __r;
}
static __inline__ uint32x2_t vmul_u32(uint32x2_t __a, uint32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint32_t)(__a[__i] * __b[__i]);
  return __r;
}
static __inline__ uint32x2_t vmul_n_u32(uint32x2_t __a, uint32_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint32_t)(__a[__i] * __b);
  return __r;
}
static __inline__ uint32x2_t vmla_u32(uint32x2_t __a, uint32x2_t __b, uint32x2_t __c) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint32_t)(__a[__i] + __b[__i] * __c[__i]);
  return __r;
}
static __inline__ uint32x2_t vmls_u32(uint32x2_t __a, uint32x2_t __b, uint32x2_t __c) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint32_t)(__a[__i] - __b[__i] * __c[__i]);
  return __r;
}
static __inline__ uint32x2_t vmla_n_u32(uint32x2_t __a, uint32x2_t __b, uint32_t __c) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint32_t)(__a[__i] + __b[__i] * __c);
  return __r;
}
static __inline__ uint32x2_t vmax_u32(uint32x2_t __a, uint32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] > __b[__i] ? __a[__i] : __b[__i];
  return __r;
}
static __inline__ uint32x2_t vmin_u32(uint32x2_t __a, uint32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] < __b[__i] ? __a[__i] : __b[__i];
  return __r;
}
static __inline__ uint32x2_t vabd_u32(uint32x2_t __a, uint32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (uint32_t)(__a[__i] > __b[__i] ? __a[__i] - __b[__i] : __b[__i] - __a[__i]);
  return __r;
}
static __inline__ uint32x2_t vceq_u32(uint32x2_t __a, uint32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] == __b[__i] ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x2_t vceqz_u32(uint32x2_t __a) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] == 0 ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x2_t vcge_u32(uint32x2_t __a, uint32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] >= __b[__i] ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x2_t vcgt_u32(uint32x2_t __a, uint32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] > __b[__i] ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x2_t vcle_u32(uint32x2_t __a, uint32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] <= __b[__i] ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x2_t vclt_u32(uint32x2_t __a, uint32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] < __b[__i] ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32_t vaddv_u32(uint32x2_t __a) {
  uint32_t __r = 0;
  for (int __i = 0; __i < 2; __i++) __r += __a[__i];
  return __r;
}
static __inline__ uint32_t vmaxv_u32(uint32x2_t __a) {
  uint32_t __r = __a[0];
  for (int __i = 1; __i < 2; __i++) if (__a[__i] > __r) __r = __a[__i];
  return __r;
}
static __inline__ uint32_t vminv_u32(uint32x2_t __a) {
  uint32_t __r = __a[0];
  for (int __i = 1; __i < 2; __i++) if (__a[__i] < __r) __r = __a[__i];
  return __r;
}
static __inline__ uint32x2_t vpadd_u32(uint32x2_t __a, uint32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = __i < 1 ? (uint32_t)(__a[2 * __i] + __a[2 * __i + 1])
        : (uint32_t)(__b[2 * (__i - 1)] + __b[2 * (__i - 1) + 1]);
  return __r;
}
static __inline__ uint64x1_t vadd_u64(uint64x1_t __a, uint64x1_t __b) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = (uint64_t)(__a[__i] + __b[__i]);
  return __r;
}
static __inline__ uint64x1_t vsub_u64(uint64x1_t __a, uint64x1_t __b) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = (uint64_t)(__a[__i] - __b[__i]);
  return __r;
}
static __inline__ uint64x1_t vceq_u64(uint64x1_t __a, uint64x1_t __b) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __a[__i] == __b[__i] ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x1_t vceqz_u64(uint64x1_t __a) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __a[__i] == 0 ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x1_t vcge_u64(uint64x1_t __a, uint64x1_t __b) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __a[__i] >= __b[__i] ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x1_t vcgt_u64(uint64x1_t __a, uint64x1_t __b) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __a[__i] > __b[__i] ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x1_t vcle_u64(uint64x1_t __a, uint64x1_t __b) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __a[__i] <= __b[__i] ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x1_t vclt_u64(uint64x1_t __a, uint64x1_t __b) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __a[__i] < __b[__i] ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ float32x2_t vadd_f32(float32x2_t __a, float32x2_t __b) {
  float32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (float32_t)(__a[__i] + __b[__i]);
  return __r;
}
static __inline__ float32x2_t vsub_f32(float32x2_t __a, float32x2_t __b) {
  float32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (float32_t)(__a[__i] - __b[__i]);
  return __r;
}
static __inline__ float32x2_t vmul_f32(float32x2_t __a, float32x2_t __b) {
  float32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (float32_t)(__a[__i] * __b[__i]);
  return __r;
}
static __inline__ float32x2_t vmul_n_f32(float32x2_t __a, float32_t __b) {
  float32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (float32_t)(__a[__i] * __b);
  return __r;
}
static __inline__ float32x2_t vmla_f32(float32x2_t __a, float32x2_t __b, float32x2_t __c) {
  float32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (float32_t)(__a[__i] + __b[__i] * __c[__i]);
  return __r;
}
static __inline__ float32x2_t vmls_f32(float32x2_t __a, float32x2_t __b, float32x2_t __c) {
  float32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (float32_t)(__a[__i] - __b[__i] * __c[__i]);
  return __r;
}
static __inline__ float32x2_t vmla_n_f32(float32x2_t __a, float32x2_t __b, float32_t __c) {
  float32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (float32_t)(__a[__i] + __b[__i] * __c);
  return __r;
}
static __inline__ float32x2_t vmax_f32(float32x2_t __a, float32x2_t __b) {
  float32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] > __b[__i] ? __a[__i] : __b[__i];
  return __r;
}
static __inline__ float32x2_t vmin_f32(float32x2_t __a, float32x2_t __b) {
  float32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] < __b[__i] ? __a[__i] : __b[__i];
  return __r;
}
static __inline__ float32x2_t vabd_f32(float32x2_t __a, float32x2_t __b) {
  float32x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (float32_t)(__a[__i] > __b[__i] ? __a[__i] - __b[__i] : __b[__i] - __a[__i]);
  return __r;
}
static __inline__ uint32x2_t vceq_f32(float32x2_t __a, float32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] == __b[__i] ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x2_t vceqz_f32(float32x2_t __a) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] == 0 ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x2_t vcge_f32(float32x2_t __a, float32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] >= __b[__i] ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x2_t vcgez_f32(float32x2_t __a) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] >= 0 ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x2_t vcgt_f32(float32x2_t __a, float32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] > __b[__i] ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x2_t vcgtz_f32(float32x2_t __a) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] > 0 ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x2_t vcle_f32(float32x2_t __a, float32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] <= __b[__i] ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x2_t vclez_f32(float32x2_t __a) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] <= 0 ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x2_t vclt_f32(float32x2_t __a, float32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] < __b[__i] ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x2_t vcltz_f32(float32x2_t __a) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] < 0 ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ float32_t vaddv_f32(float32x2_t __a) {
  float32_t __r = 0;
  for (int __i = 0; __i < 2; __i++) __r += __a[__i];
  return __r;
}
static __inline__ float32_t vmaxv_f32(float32x2_t __a) {
  float32_t __r = __a[0];
  for (int __i = 1; __i < 2; __i++) if (__a[__i] > __r) __r = __a[__i];
  return __r;
}
static __inline__ float32_t vminv_f32(float32x2_t __a) {
  float32_t __r = __a[0];
  for (int __i = 1; __i < 2; __i++) if (__a[__i] < __r) __r = __a[__i];
  return __r;
}
static __inline__ float32x2_t vpadd_f32(float32x2_t __a, float32x2_t __b) {
  float32x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = __i < 1 ? (float32_t)(__a[2 * __i] + __a[2 * __i + 1])
        : (float32_t)(__b[2 * (__i - 1)] + __b[2 * (__i - 1) + 1]);
  return __r;
}
static __inline__ float64x1_t vadd_f64(float64x1_t __a, float64x1_t __b) {
  float64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = (float64_t)(__a[__i] + __b[__i]);
  return __r;
}
static __inline__ float64x1_t vsub_f64(float64x1_t __a, float64x1_t __b) {
  float64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = (float64_t)(__a[__i] - __b[__i]);
  return __r;
}
static __inline__ float64x1_t vmul_f64(float64x1_t __a, float64x1_t __b) {
  float64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = (float64_t)(__a[__i] * __b[__i]);
  return __r;
}
static __inline__ float64x1_t vmul_n_f64(float64x1_t __a, float64_t __b) {
  float64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = (float64_t)(__a[__i] * __b);
  return __r;
}
static __inline__ float64x1_t vmla_f64(float64x1_t __a, float64x1_t __b, float64x1_t __c) {
  float64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = (float64_t)(__a[__i] + __b[__i] * __c[__i]);
  return __r;
}
static __inline__ float64x1_t vmls_f64(float64x1_t __a, float64x1_t __b, float64x1_t __c) {
  float64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = (float64_t)(__a[__i] - __b[__i] * __c[__i]);
  return __r;
}
static __inline__ float64x1_t vmax_f64(float64x1_t __a, float64x1_t __b) {
  float64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __a[__i] > __b[__i] ? __a[__i] : __b[__i];
  return __r;
}
static __inline__ float64x1_t vmin_f64(float64x1_t __a, float64x1_t __b) {
  float64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __a[__i] < __b[__i] ? __a[__i] : __b[__i];
  return __r;
}
static __inline__ float64x1_t vabd_f64(float64x1_t __a, float64x1_t __b) {
  float64x1_t __r;
  for (int __i = 0; __i < 1; __i++)
    __r[__i] = (float64_t)(__a[__i] > __b[__i] ? __a[__i] - __b[__i] : __b[__i] - __a[__i]);
  return __r;
}
static __inline__ uint64x1_t vceq_f64(float64x1_t __a, float64x1_t __b) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __a[__i] == __b[__i] ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x1_t vceqz_f64(float64x1_t __a) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __a[__i] == 0 ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x1_t vcge_f64(float64x1_t __a, float64x1_t __b) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __a[__i] >= __b[__i] ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x1_t vcgez_f64(float64x1_t __a) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __a[__i] >= 0 ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x1_t vcgt_f64(float64x1_t __a, float64x1_t __b) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __a[__i] > __b[__i] ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x1_t vcgtz_f64(float64x1_t __a) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __a[__i] > 0 ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x1_t vcle_f64(float64x1_t __a, float64x1_t __b) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __a[__i] <= __b[__i] ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x1_t vclez_f64(float64x1_t __a) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __a[__i] <= 0 ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x1_t vclt_f64(float64x1_t __a, float64x1_t __b) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __a[__i] < __b[__i] ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x1_t vcltz_f64(float64x1_t __a) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __a[__i] < 0 ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ int8x8_t vneg_s8(int8x8_t __a) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int8_t)-__a[__i];
  return __r;
}
static __inline__ int8x8_t vabs_s8(int8x8_t __a) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] < 0 ? (int8_t)-__a[__i] : __a[__i];
  return __r;
}
static __inline__ int16x4_t vneg_s16(int16x4_t __a) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int16_t)-__a[__i];
  return __r;
}
static __inline__ int16x4_t vabs_s16(int16x4_t __a) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] < 0 ? (int16_t)-__a[__i] : __a[__i];
  return __r;
}
static __inline__ int32x2_t vneg_s32(int32x2_t __a) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int32_t)-__a[__i];
  return __r;
}
static __inline__ int32x2_t vabs_s32(int32x2_t __a) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] < 0 ? (int32_t)-__a[__i] : __a[__i];
  return __r;
}
static __inline__ int64x1_t vneg_s64(int64x1_t __a) {
  int64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = (int64_t)-__a[__i];
  return __r;
}
static __inline__ int64x1_t vabs_s64(int64x1_t __a) {
  int64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __a[__i] < 0 ? (int64_t)-__a[__i] : __a[__i];
  return __r;
}
static __inline__ float32x2_t vneg_f32(float32x2_t __a) {
  float32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (float32_t)-__a[__i];
  return __r;
}
static __inline__ float32x2_t vabs_f32(float32x2_t __a) {
  float32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] < 0 ? (float32_t)-__a[__i] : __a[__i];
  return __r;
}
static __inline__ float64x1_t vneg_f64(float64x1_t __a) {
  float64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = (float64_t)-__a[__i];
  return __r;
}
static __inline__ float64x1_t vabs_f64(float64x1_t __a) {
  float64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __a[__i] < 0 ? (float64_t)-__a[__i] : __a[__i];
  return __r;
}
static __inline__ float32x2_t vdiv_f32(float32x2_t __a, float32x2_t __b) {
  float32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] / __b[__i];
  return __r;
}
static __inline__ float64x1_t vdiv_f64(float64x1_t __a, float64x1_t __b) {
  float64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __a[__i] / __b[__i];
  return __r;
}
static __inline__ int8x8_t vand_s8(int8x8_t __a, int8x8_t __b) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int8_t)(__a[__i] & __b[__i]);
  return __r;
}
static __inline__ int8x8_t vorr_s8(int8x8_t __a, int8x8_t __b) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int8_t)(__a[__i] | __b[__i]);
  return __r;
}
static __inline__ int8x8_t veor_s8(int8x8_t __a, int8x8_t __b) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int8_t)(__a[__i] ^ __b[__i]);
  return __r;
}
static __inline__ int8x8_t vbic_s8(int8x8_t __a, int8x8_t __b) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int8_t)(__a[__i] & ~__b[__i]);
  return __r;
}
static __inline__ int8x8_t vorn_s8(int8x8_t __a, int8x8_t __b) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int8_t)(__a[__i] | ~__b[__i]);
  return __r;
}
static __inline__ int8x8_t vmvn_s8(int8x8_t __a) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int8_t)~__a[__i];
  return __r;
}
static __inline__ uint8x8_t vtst_s8(int8x8_t __a, int8x8_t __b) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (__a[__i] & __b[__i]) ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ int16x4_t vand_s16(int16x4_t __a, int16x4_t __b) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int16_t)(__a[__i] & __b[__i]);
  return __r;
}
static __inline__ int16x4_t vorr_s16(int16x4_t __a, int16x4_t __b) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int16_t)(__a[__i] | __b[__i]);
  return __r;
}
static __inline__ int16x4_t veor_s16(int16x4_t __a, int16x4_t __b) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int16_t)(__a[__i] ^ __b[__i]);
  return __r;
}
static __inline__ int16x4_t vbic_s16(int16x4_t __a, int16x4_t __b) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int16_t)(__a[__i] & ~__b[__i]);
  return __r;
}
static __inline__ int16x4_t vorn_s16(int16x4_t __a, int16x4_t __b) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int16_t)(__a[__i] | ~__b[__i]);
  return __r;
}
static __inline__ int16x4_t vmvn_s16(int16x4_t __a) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int16_t)~__a[__i];
  return __r;
}
static __inline__ uint16x4_t vtst_s16(int16x4_t __a, int16x4_t __b) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (__a[__i] & __b[__i]) ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ int32x2_t vand_s32(int32x2_t __a, int32x2_t __b) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int32_t)(__a[__i] & __b[__i]);
  return __r;
}
static __inline__ int32x2_t vorr_s32(int32x2_t __a, int32x2_t __b) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int32_t)(__a[__i] | __b[__i]);
  return __r;
}
static __inline__ int32x2_t veor_s32(int32x2_t __a, int32x2_t __b) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int32_t)(__a[__i] ^ __b[__i]);
  return __r;
}
static __inline__ int32x2_t vbic_s32(int32x2_t __a, int32x2_t __b) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int32_t)(__a[__i] & ~__b[__i]);
  return __r;
}
static __inline__ int32x2_t vorn_s32(int32x2_t __a, int32x2_t __b) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int32_t)(__a[__i] | ~__b[__i]);
  return __r;
}
static __inline__ int32x2_t vmvn_s32(int32x2_t __a) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int32_t)~__a[__i];
  return __r;
}
static __inline__ uint32x2_t vtst_s32(int32x2_t __a, int32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (__a[__i] & __b[__i]) ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ int64x1_t vand_s64(int64x1_t __a, int64x1_t __b) {
  int64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = (int64_t)(__a[__i] & __b[__i]);
  return __r;
}
static __inline__ int64x1_t vorr_s64(int64x1_t __a, int64x1_t __b) {
  int64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = (int64_t)(__a[__i] | __b[__i]);
  return __r;
}
static __inline__ int64x1_t veor_s64(int64x1_t __a, int64x1_t __b) {
  int64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = (int64_t)(__a[__i] ^ __b[__i]);
  return __r;
}
static __inline__ int64x1_t vbic_s64(int64x1_t __a, int64x1_t __b) {
  int64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = (int64_t)(__a[__i] & ~__b[__i]);
  return __r;
}
static __inline__ int64x1_t vorn_s64(int64x1_t __a, int64x1_t __b) {
  int64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = (int64_t)(__a[__i] | ~__b[__i]);
  return __r;
}
static __inline__ uint64x1_t vtst_s64(int64x1_t __a, int64x1_t __b) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = (__a[__i] & __b[__i]) ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint8x8_t vand_u8(uint8x8_t __a, uint8x8_t __b) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint8_t)(__a[__i] & __b[__i]);
  return __r;
}
static __inline__ uint8x8_t vorr_u8(uint8x8_t __a, uint8x8_t __b) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint8_t)(__a[__i] | __b[__i]);
  return __r;
}
static __inline__ uint8x8_t veor_u8(uint8x8_t __a, uint8x8_t __b) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint8_t)(__a[__i] ^ __b[__i]);
  return __r;
}
static __inline__ uint8x8_t vbic_u8(uint8x8_t __a, uint8x8_t __b) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint8_t)(__a[__i] & ~__b[__i]);
  return __r;
}
static __inline__ uint8x8_t vorn_u8(uint8x8_t __a, uint8x8_t __b) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint8_t)(__a[__i] | ~__b[__i]);
  return __r;
}
static __inline__ uint8x8_t vmvn_u8(uint8x8_t __a) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint8_t)~__a[__i];
  return __r;
}
static __inline__ uint8x8_t vtst_u8(uint8x8_t __a, uint8x8_t __b) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (__a[__i] & __b[__i]) ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ uint16x4_t vand_u16(uint16x4_t __a, uint16x4_t __b) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint16_t)(__a[__i] & __b[__i]);
  return __r;
}
static __inline__ uint16x4_t vorr_u16(uint16x4_t __a, uint16x4_t __b) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint16_t)(__a[__i] | __b[__i]);
  return __r;
}
static __inline__ uint16x4_t veor_u16(uint16x4_t __a, uint16x4_t __b) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint16_t)(__a[__i] ^ __b[__i]);
  return __r;
}
static __inline__ uint16x4_t vbic_u16(uint16x4_t __a, uint16x4_t __b) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint16_t)(__a[__i] & ~__b[__i]);
  return __r;
}
static __inline__ uint16x4_t vorn_u16(uint16x4_t __a, uint16x4_t __b) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint16_t)(__a[__i] | ~__b[__i]);
  return __r;
}
static __inline__ uint16x4_t vmvn_u16(uint16x4_t __a) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint16_t)~__a[__i];
  return __r;
}
static __inline__ uint16x4_t vtst_u16(uint16x4_t __a, uint16x4_t __b) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (__a[__i] & __b[__i]) ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ uint32x2_t vand_u32(uint32x2_t __a, uint32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint32_t)(__a[__i] & __b[__i]);
  return __r;
}
static __inline__ uint32x2_t vorr_u32(uint32x2_t __a, uint32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint32_t)(__a[__i] | __b[__i]);
  return __r;
}
static __inline__ uint32x2_t veor_u32(uint32x2_t __a, uint32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint32_t)(__a[__i] ^ __b[__i]);
  return __r;
}
static __inline__ uint32x2_t vbic_u32(uint32x2_t __a, uint32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint32_t)(__a[__i] & ~__b[__i]);
  return __r;
}
static __inline__ uint32x2_t vorn_u32(uint32x2_t __a, uint32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint32_t)(__a[__i] | ~__b[__i]);
  return __r;
}
static __inline__ uint32x2_t vmvn_u32(uint32x2_t __a) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint32_t)~__a[__i];
  return __r;
}
static __inline__ uint32x2_t vtst_u32(uint32x2_t __a, uint32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (__a[__i] & __b[__i]) ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint64x1_t vand_u64(uint64x1_t __a, uint64x1_t __b) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = (uint64_t)(__a[__i] & __b[__i]);
  return __r;
}
static __inline__ uint64x1_t vorr_u64(uint64x1_t __a, uint64x1_t __b) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = (uint64_t)(__a[__i] | __b[__i]);
  return __r;
}
static __inline__ uint64x1_t veor_u64(uint64x1_t __a, uint64x1_t __b) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = (uint64_t)(__a[__i] ^ __b[__i]);
  return __r;
}
static __inline__ uint64x1_t vbic_u64(uint64x1_t __a, uint64x1_t __b) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = (uint64_t)(__a[__i] & ~__b[__i]);
  return __r;
}
static __inline__ uint64x1_t vorn_u64(uint64x1_t __a, uint64x1_t __b) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = (uint64_t)(__a[__i] | ~__b[__i]);
  return __r;
}
static __inline__ uint64x1_t vtst_u64(uint64x1_t __a, uint64x1_t __b) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = (__a[__i] & __b[__i]) ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ poly8x8_t vmvn_p8(poly8x8_t __a) {
  poly8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (poly8_t)~__a[__i];
  return __r;
}
static __inline__ uint8x8_t vtst_p8(poly8x8_t __a, poly8x8_t __b) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (__a[__i] & __b[__i]) ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ uint16x4_t vtst_p16(poly16x4_t __a, poly16x4_t __b) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (__a[__i] & __b[__i]) ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ uint64x1_t vtst_p64(poly64x1_t __a, poly64x1_t __b) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = (__a[__i] & __b[__i]) ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ int8x8_t vbsl_s8(uint8x8_t __m, int8x8_t __a, int8x8_t __b) {
  uint8x8_t __x, __y;
  __builtin_memcpy(&__x, &__a, sizeof __x);
  __builtin_memcpy(&__y, &__b, sizeof __y);
  __x = (__x & __m) | (__y & ~__m);
  int8x8_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ int16x4_t vbsl_s16(uint16x4_t __m, int16x4_t __a, int16x4_t __b) {
  uint16x4_t __x, __y;
  __builtin_memcpy(&__x, &__a, sizeof __x);
  __builtin_memcpy(&__y, &__b, sizeof __y);
  __x = (__x & __m) | (__y & ~__m);
  int16x4_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ int32x2_t vbsl_s32(uint32x2_t __m, int32x2_t __a, int32x2_t __b) {
  uint32x2_t __x, __y;
  __builtin_memcpy(&__x, &__a, sizeof __x);
  __builtin_memcpy(&__y, &__b, sizeof __y);
  __x = (__x & __m) | (__y & ~__m);
  int32x2_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ int64x1_t vbsl_s64(uint64x1_t __m, int64x1_t __a, int64x1_t __b) {
  uint64x1_t __x, __y;
  __builtin_memcpy(&__x, &__a, sizeof __x);
  __builtin_memcpy(&__y, &__b, sizeof __y);
  __x = (__x & __m) | (__y & ~__m);
  int64x1_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ uint8x8_t vbsl_u8(uint8x8_t __m, uint8x8_t __a, uint8x8_t __b) {
  uint8x8_t __x, __y;
  __builtin_memcpy(&__x, &__a, sizeof __x);
  __builtin_memcpy(&__y, &__b, sizeof __y);
  __x = (__x & __m) | (__y & ~__m);
  uint8x8_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ uint16x4_t vbsl_u16(uint16x4_t __m, uint16x4_t __a, uint16x4_t __b) {
  uint16x4_t __x, __y;
  __builtin_memcpy(&__x, &__a, sizeof __x);
  __builtin_memcpy(&__y, &__b, sizeof __y);
  __x = (__x & __m) | (__y & ~__m);
  uint16x4_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ uint32x2_t vbsl_u32(uint32x2_t __m, uint32x2_t __a, uint32x2_t __b) {
  uint32x2_t __x, __y;
  __builtin_memcpy(&__x, &__a, sizeof __x);
  __builtin_memcpy(&__y, &__b, sizeof __y);
  __x = (__x & __m) | (__y & ~__m);
  uint32x2_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ uint64x1_t vbsl_u64(uint64x1_t __m, uint64x1_t __a, uint64x1_t __b) {
  uint64x1_t __x, __y;
  __builtin_memcpy(&__x, &__a, sizeof __x);
  __builtin_memcpy(&__y, &__b, sizeof __y);
  __x = (__x & __m) | (__y & ~__m);
  uint64x1_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ float32x2_t vbsl_f32(uint32x2_t __m, float32x2_t __a, float32x2_t __b) {
  uint32x2_t __x, __y;
  __builtin_memcpy(&__x, &__a, sizeof __x);
  __builtin_memcpy(&__y, &__b, sizeof __y);
  __x = (__x & __m) | (__y & ~__m);
  float32x2_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ float64x1_t vbsl_f64(uint64x1_t __m, float64x1_t __a, float64x1_t __b) {
  uint64x1_t __x, __y;
  __builtin_memcpy(&__x, &__a, sizeof __x);
  __builtin_memcpy(&__y, &__b, sizeof __y);
  __x = (__x & __m) | (__y & ~__m);
  float64x1_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ poly8x8_t vbsl_p8(uint8x8_t __m, poly8x8_t __a, poly8x8_t __b) {
  uint8x8_t __x, __y;
  __builtin_memcpy(&__x, &__a, sizeof __x);
  __builtin_memcpy(&__y, &__b, sizeof __y);
  __x = (__x & __m) | (__y & ~__m);
  poly8x8_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ poly16x4_t vbsl_p16(uint16x4_t __m, poly16x4_t __a, poly16x4_t __b) {
  uint16x4_t __x, __y;
  __builtin_memcpy(&__x, &__a, sizeof __x);
  __builtin_memcpy(&__y, &__b, sizeof __y);
  __x = (__x & __m) | (__y & ~__m);
  poly16x4_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ poly64x1_t vbsl_p64(uint64x1_t __m, poly64x1_t __a, poly64x1_t __b) {
  uint64x1_t __x, __y;
  __builtin_memcpy(&__x, &__a, sizeof __x);
  __builtin_memcpy(&__y, &__b, sizeof __y);
  __x = (__x & __m) | (__y & ~__m);
  poly64x1_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ int8x8_t vqadd_s8(int8x8_t __a, int8x8_t __b) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (int8_t)((int64_t)__a[__i] + (int64_t)__b[__i] > ((int64_t)1 << 7) - 1
        ? ((int64_t)1 << 7) - 1 : (int64_t)__a[__i] + (int64_t)__b[__i] < -((int64_t)1 << 7)
        ? -((int64_t)1 << 7) : (int64_t)__a[__i] + (int64_t)__b[__i]);
  return __r;
}
static __inline__ int8x8_t vqsub_s8(int8x8_t __a, int8x8_t __b) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (int8_t)((int64_t)__a[__i] - (int64_t)__b[__i] > ((int64_t)1 << 7) - 1
        ? ((int64_t)1 << 7) - 1 : (int64_t)__a[__i] - (int64_t)__b[__i] < -((int64_t)1 << 7)
        ? -((int64_t)1 << 7) : (int64_t)__a[__i] - (int64_t)__b[__i]);
  return __r;
}
static __inline__ int8x8_t vhadd_s8(int8x8_t __a, int8x8_t __b) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int8_t)(((int64_t)__a[__i] + __b[__i]) >> 1);
  return __r;
}
static __inline__ int8x8_t vrhadd_s8(int8x8_t __a, int8x8_t __b) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int8_t)(((int64_t)__a[__i] + __b[__i] + 1) >> 1);
  return __r;
}
static __inline__ int8x8_t vshl_n_s8(int8x8_t __a, const int __n) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int8_t)((uint64_t)__a[__i] << __n);
  return __r;
}
static __inline__ int8x8_t vshr_n_s8(int8x8_t __a, const int __n) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = __n == 8 ? (int8_t)(__a[__i] < 0 ? -1 : 0) : (int8_t)(__a[__i] >> __n);
  return __r;
}
static __inline__ int8x8_t vsra_n_s8(int8x8_t __a, int8x8_t __b, const int __n) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (int8_t)(__a[__i] + (__n == 8 ? (int8_t)(__b[__i] < 0 ? -1 : 0)
        : (int8_t)(__b[__i] >> __n)));
  return __r;
}
static __inline__ int8x8_t vshl_s8(int8x8_t __a, int8x8_t __b) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (int8_t)__b[__i] >= 8 ? 0 : (int8_t)__b[__i] <= -8 ? (int8_t)(__a[__i] < 0 ? -1
        : 0) : (int8_t)__b[__i] >= 0 ? (int8_t)((uint64_t)__a[__i] << (int8_t)__b[__i])
        : (int8_t)(__a[__i] >> -(int8_t)__b[__i]);
  return __r;
}
static __inline__ int8x8_t vsli_n_s8(int8x8_t __a, int8x8_t __b, const int __n) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (int8_t)(((uint64_t)__b[__i] << __n)
        | ((uint64_t)__a[__i] & (((uint64_t)-1 >> 56) >> (8 - __n))));
  return __r;
}
static __inline__ int8x8_t vsri_n_s8(int8x8_t __a, int8x8_t __b, const int __n) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (int8_t)((((uint64_t)__b[__i] & ((uint64_t)-1 >> 56)) >> __n)
        | ((uint64_t)__a[__i] & ~(((uint64_t)-1 >> 56) >> __n)));
  return __r;
}
static __inline__ int8x8_t vclz_s8(int8x8_t __a) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (int8_t)(__a[__i] == 0 ? 8
        : __builtin_clz((uint32_t)__a[__i] & ((uint64_t)-1 >> 56)) - 24);
  return __r;
}
static __inline__ int8x8_t vcnt_s8(int8x8_t __a) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int8_t)__builtin_popcount((uint8_t)__a[__i]);
  return __r;
}
static __inline__ int8x8_t vrbit_s8(int8x8_t __a) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (int8_t)(((((uint8_t)__a[__i] * 0x0802u & 0x22110u)
        | ((uint8_t)__a[__i] * 0x8020u & 0x88440u)) * 0x10101u >> 16) & 0xff);
  return __r;
}
static __inline__ int16x4_t vqadd_s16(int16x4_t __a, int16x4_t __b) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (int16_t)((int64_t)__a[__i] + (int64_t)__b[__i] > ((int64_t)1 << 15) - 1
        ? ((int64_t)1 << 15) - 1 : (int64_t)__a[__i] + (int64_t)__b[__i] < -((int64_t)1 << 15)
        ? -((int64_t)1 << 15) : (int64_t)__a[__i] + (int64_t)__b[__i]);
  return __r;
}
static __inline__ int16x4_t vqsub_s16(int16x4_t __a, int16x4_t __b) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (int16_t)((int64_t)__a[__i] - (int64_t)__b[__i] > ((int64_t)1 << 15) - 1
        ? ((int64_t)1 << 15) - 1 : (int64_t)__a[__i] - (int64_t)__b[__i] < -((int64_t)1 << 15)
        ? -((int64_t)1 << 15) : (int64_t)__a[__i] - (int64_t)__b[__i]);
  return __r;
}
static __inline__ int16x4_t vhadd_s16(int16x4_t __a, int16x4_t __b) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int16_t)(((int64_t)__a[__i] + __b[__i]) >> 1);
  return __r;
}
static __inline__ int16x4_t vrhadd_s16(int16x4_t __a, int16x4_t __b) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int16_t)(((int64_t)__a[__i] + __b[__i] + 1) >> 1);
  return __r;
}
static __inline__ int16x4_t vshl_n_s16(int16x4_t __a, const int __n) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int16_t)((uint64_t)__a[__i] << __n);
  return __r;
}
static __inline__ int16x4_t vshr_n_s16(int16x4_t __a, const int __n) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = __n == 16 ? (int16_t)(__a[__i] < 0 ? -1 : 0) : (int16_t)(__a[__i] >> __n);
  return __r;
}
static __inline__ int16x4_t vsra_n_s16(int16x4_t __a, int16x4_t __b, const int __n) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (int16_t)(__a[__i] + (__n == 16 ? (int16_t)(__b[__i] < 0 ? -1 : 0)
        : (int16_t)(__b[__i] >> __n)));
  return __r;
}
static __inline__ int16x4_t vshl_s16(int16x4_t __a, int16x4_t __b) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (int8_t)__b[__i] >= 16 ? 0 : (int8_t)__b[__i] <= -16 ? (int16_t)(__a[__i] < 0 ? -1
        : 0) : (int8_t)__b[__i] >= 0 ? (int16_t)((uint64_t)__a[__i] << (int8_t)__b[__i])
        : (int16_t)(__a[__i] >> -(int8_t)__b[__i]);
  return __r;
}
static __inline__ int16x4_t vsli_n_s16(int16x4_t __a, int16x4_t __b, const int __n) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (int16_t)(((uint64_t)__b[__i] << __n)
        | ((uint64_t)__a[__i] & (((uint64_t)-1 >> 48) >> (16 - __n))));
  return __r;
}
static __inline__ int16x4_t vsri_n_s16(int16x4_t __a, int16x4_t __b, const int __n) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (int16_t)((((uint64_t)__b[__i] & ((uint64_t)-1 >> 48)) >> __n)
        | ((uint64_t)__a[__i] & ~(((uint64_t)-1 >> 48) >> __n)));
  return __r;
}
static __inline__ int16x4_t vclz_s16(int16x4_t __a) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (int16_t)(__a[__i] == 0 ? 16
        : __builtin_clz((uint32_t)__a[__i] & ((uint64_t)-1 >> 48)) - 16);
  return __r;
}
static __inline__ int32x2_t vqadd_s32(int32x2_t __a, int32x2_t __b) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (int32_t)((int64_t)__a[__i] + (int64_t)__b[__i] > ((int64_t)1 << 31) - 1
        ? ((int64_t)1 << 31) - 1 : (int64_t)__a[__i] + (int64_t)__b[__i] < -((int64_t)1 << 31)
        ? -((int64_t)1 << 31) : (int64_t)__a[__i] + (int64_t)__b[__i]);
  return __r;
}
static __inline__ int32x2_t vqsub_s32(int32x2_t __a, int32x2_t __b) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (int32_t)((int64_t)__a[__i] - (int64_t)__b[__i] > ((int64_t)1 << 31) - 1
        ? ((int64_t)1 << 31) - 1 : (int64_t)__a[__i] - (int64_t)__b[__i] < -((int64_t)1 << 31)
        ? -((int64_t)1 << 31) : (int64_t)__a[__i] - (int64_t)__b[__i]);
  return __r;
}
static __inline__ int32x2_t vhadd_s32(int32x2_t __a, int32x2_t __b) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int32_t)(((int64_t)__a[__i] + __b[__i]) >> 1);
  return __r;
}
static __inline__ int32x2_t vrhadd_s32(int32x2_t __a, int32x2_t __b) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int32_t)(((int64_t)__a[__i] + __b[__i] + 1) >> 1);
  return __r;
}
static __inline__ int32x2_t vshl_n_s32(int32x2_t __a, const int __n) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int32_t)((uint64_t)__a[__i] << __n);
  return __r;
}
static __inline__ int32x2_t vshr_n_s32(int32x2_t __a, const int __n) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = __n == 32 ? (int32_t)(__a[__i] < 0 ? -1 : 0) : (int32_t)(__a[__i] >> __n);
  return __r;
}
static __inline__ int32x2_t vsra_n_s32(int32x2_t __a, int32x2_t __b, const int __n) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (int32_t)(__a[__i] + (__n == 32 ? (int32_t)(__b[__i] < 0 ? -1 : 0)
        : (int32_t)(__b[__i] >> __n)));
  return __r;
}
static __inline__ int32x2_t vshl_s32(int32x2_t __a, int32x2_t __b) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (int8_t)__b[__i] >= 32 ? 0 : (int8_t)__b[__i] <= -32 ? (int32_t)(__a[__i] < 0 ? -1
        : 0) : (int8_t)__b[__i] >= 0 ? (int32_t)((uint64_t)__a[__i] << (int8_t)__b[__i])
        : (int32_t)(__a[__i] >> -(int8_t)__b[__i]);
  return __r;
}
static __inline__ int32x2_t vsli_n_s32(int32x2_t __a, int32x2_t __b, const int __n) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (int32_t)(((uint64_t)__b[__i] << __n)
        | ((uint64_t)__a[__i] & (((uint64_t)-1 >> 32) >> (32 - __n))));
  return __r;
}
static __inline__ int32x2_t vsri_n_s32(int32x2_t __a, int32x2_t __b, const int __n) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (int32_t)((((uint64_t)__b[__i] & ((uint64_t)-1 >> 32)) >> __n)
        | ((uint64_t)__a[__i] & ~(((uint64_t)-1 >> 32) >> __n)));
  return __r;
}
static __inline__ int32x2_t vclz_s32(int32x2_t __a) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (int32_t)(__a[__i] == 0 ? 32
        : __builtin_clz((uint32_t)__a[__i] & ((uint64_t)-1 >> 32)) - 0);
  return __r;
}
static __inline__ int64x1_t vqadd_s64(int64x1_t __a, int64x1_t __b) {
  int64x1_t __r;
  for (int __i = 0; __i < 1; __i++)
    __r[__i] = __b[__i] > 0 && __a[__i] > INT64_MAX - __b[__i] ? INT64_MAX
        : __b[__i] < 0 && __a[__i] < INT64_MIN - __b[__i] ? INT64_MIN : __a[__i] + __b[__i];
  return __r;
}
static __inline__ int64x1_t vqsub_s64(int64x1_t __a, int64x1_t __b) {
  int64x1_t __r;
  for (int __i = 0; __i < 1; __i++)
    __r[__i] = __b[__i] < 0 && __a[__i] > INT64_MAX + __b[__i] ? INT64_MAX
        : __b[__i] > 0 && __a[__i] < INT64_MIN + __b[__i] ? INT64_MIN : __a[__i] - __b[__i];
  return __r;
}
static __inline__ int64x1_t vshl_n_s64(int64x1_t __a, const int __n) {
  int64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = (int64_t)((uint64_t)__a[__i] << __n);
  return __r;
}
static __inline__ int64x1_t vshr_n_s64(int64x1_t __a, const int __n) {
  int64x1_t __r;
  for (int __i = 0; __i < 1; __i++)
    __r[__i] = __n == 64 ? (int64_t)(__a[__i] < 0 ? -1 : 0) : (int64_t)(__a[__i] >> __n);
  return __r;
}
static __inline__ int64x1_t vsra_n_s64(int64x1_t __a, int64x1_t __b, const int __n) {
  int64x1_t __r;
  for (int __i = 0; __i < 1; __i++)
    __r[__i] = (int64_t)(__a[__i] + (__n == 64 ? (int64_t)(__b[__i] < 0 ? -1 : 0)
        : (int64_t)(__b[__i] >> __n)));
  return __r;
}
static __inline__ int64x1_t vshl_s64(int64x1_t __a, int64x1_t __b) {
  int64x1_t __r;
  for (int __i = 0; __i < 1; __i++)
    __r[__i] = (int8_t)__b[__i] >= 64 ? 0 : (int8_t)__b[__i] <= -64 ? (int64_t)(__a[__i] < 0 ? -1
        : 0) : (int8_t)__b[__i] >= 0 ? (int64_t)((uint64_t)__a[__i] << (int8_t)__b[__i])
        : (int64_t)(__a[__i] >> -(int8_t)__b[__i]);
  return __r;
}
static __inline__ int64x1_t vsli_n_s64(int64x1_t __a, int64x1_t __b, const int __n) {
  int64x1_t __r;
  for (int __i = 0; __i < 1; __i++)
    __r[__i] = (int64_t)(((uint64_t)__b[__i] << __n)
        | ((uint64_t)__a[__i] & (((uint64_t)-1 >> 0) >> (64 - __n))));
  return __r;
}
static __inline__ int64x1_t vsri_n_s64(int64x1_t __a, int64x1_t __b, const int __n) {
  int64x1_t __r;
  for (int __i = 0; __i < 1; __i++)
    __r[__i] = (int64_t)((((uint64_t)__b[__i] & ((uint64_t)-1 >> 0)) >> __n)
        | ((uint64_t)__a[__i] & ~(((uint64_t)-1 >> 0) >> __n)));
  return __r;
}
static __inline__ uint8x8_t vqadd_u8(uint8x8_t __a, uint8x8_t __b) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (uint8_t)((int64_t)__a[__i] + (int64_t)__b[__i] > ((int64_t)1 << 8) - 1
        ? ((int64_t)1 << 8) - 1 : (int64_t)__a[__i] + (int64_t)__b[__i] < 0 ? 0
        : (int64_t)__a[__i] + (int64_t)__b[__i]);
  return __r;
}
static __inline__ uint8x8_t vqsub_u8(uint8x8_t __a, uint8x8_t __b) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (uint8_t)((int64_t)__a[__i] - (int64_t)__b[__i] > ((int64_t)1 << 8) - 1
        ? ((int64_t)1 << 8) - 1 : (int64_t)__a[__i] - (int64_t)__b[__i] < 0 ? 0
        : (int64_t)__a[__i] - (int64_t)__b[__i]);
  return __r;
}
static __inline__ uint8x8_t vhadd_u8(uint8x8_t __a, uint8x8_t __b) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint8_t)(((int64_t)__a[__i] + __b[__i]) >> 1);
  return __r;
}
static __inline__ uint8x8_t vrhadd_u8(uint8x8_t __a, uint8x8_t __b) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint8_t)(((int64_t)__a[__i] + __b[__i] + 1) >> 1);
  return __r;
}
static __inline__ uint8x8_t vshl_n_u8(uint8x8_t __a, const int __n) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint8_t)((uint64_t)__a[__i] << __n);
  return __r;
}
static __inline__ uint8x8_t vshr_n_u8(uint8x8_t __a, const int __n) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __n == 8 ? (uint8_t)(0) : (uint8_t)(__a[__i] >> __n);
  return __r;
}
static __inline__ uint8x8_t vsra_n_u8(uint8x8_t __a, uint8x8_t __b, const int __n) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (uint8_t)(__a[__i] + (__n == 8 ? (uint8_t)(0) : (uint8_t)(__b[__i] >> __n)));
  return __r;
}
static __inline__ uint8x8_t vshl_u8(uint8x8_t __a, int8x8_t __b) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (int8_t)__b[__i] >= 8 ? 0 : (int8_t)__b[__i] <= -8 ? (uint8_t)(0)
        : (int8_t)__b[__i] >= 0 ? (uint8_t)((uint64_t)__a[__i] << (int8_t)__b[__i])
        : (uint8_t)(__a[__i] >> -(int8_t)__b[__i]);
  return __r;
}
static __inline__ uint8x8_t vsli_n_u8(uint8x8_t __a, uint8x8_t __b, const int __n) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (uint8_t)(((uint64_t)__b[__i] << __n)
        | ((uint64_t)__a[__i] & (((uint64_t)-1 >> 56) >> (8 - __n))));
  return __r;
}
static __inline__ uint8x8_t vsri_n_u8(uint8x8_t __a, uint8x8_t __b, const int __n) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (uint8_t)((((uint64_t)__b[__i] & ((uint64_t)-1 >> 56)) >> __n)
        | ((uint64_t)__a[__i] & ~(((uint64_t)-1 >> 56) >> __n)));
  return __r;
}
static __inline__ uint8x8_t vclz_u8(uint8x8_t __a) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (uint8_t)(__a[__i] == 0 ? 8
        : __builtin_clz((uint32_t)__a[__i] & ((uint64_t)-1 >> 56)) - 24);
  return __r;
}
static __inline__ uint8x8_t vcnt_u8(uint8x8_t __a) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint8_t)__builtin_popcount((uint8_t)__a[__i]);
  return __r;
}
static __inline__ uint8x8_t vrbit_u8(uint8x8_t __a) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (uint8_t)(((((uint8_t)__a[__i] * 0x0802u & 0x22110u)
        | ((uint8_t)__a[__i] * 0x8020u & 0x88440u)) * 0x10101u >> 16) & 0xff);
  return __r;
}
static __inline__ uint16x4_t vqadd_u16(uint16x4_t __a, uint16x4_t __b) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (uint16_t)((int64_t)__a[__i] + (int64_t)__b[__i] > ((int64_t)1 << 16) - 1
        ? ((int64_t)1 << 16) - 1 : (int64_t)__a[__i] + (int64_t)__b[__i] < 0 ? 0
        : (int64_t)__a[__i] + (int64_t)__b[__i]);
  return __r;
}
static __inline__ uint16x4_t vqsub_u16(uint16x4_t __a, uint16x4_t __b) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (uint16_t)((int64_t)__a[__i] - (int64_t)__b[__i] > ((int64_t)1 << 16) - 1
        ? ((int64_t)1 << 16) - 1 : (int64_t)__a[__i] - (int64_t)__b[__i] < 0 ? 0
        : (int64_t)__a[__i] - (int64_t)__b[__i]);
  return __r;
}
static __inline__ uint16x4_t vhadd_u16(uint16x4_t __a, uint16x4_t __b) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint16_t)(((int64_t)__a[__i] + __b[__i]) >> 1);
  return __r;
}
static __inline__ uint16x4_t vrhadd_u16(uint16x4_t __a, uint16x4_t __b) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint16_t)(((int64_t)__a[__i] + __b[__i] + 1) >> 1);
  return __r;
}
static __inline__ uint16x4_t vshl_n_u16(uint16x4_t __a, const int __n) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint16_t)((uint64_t)__a[__i] << __n);
  return __r;
}
static __inline__ uint16x4_t vshr_n_u16(uint16x4_t __a, const int __n) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = __n == 16 ? (uint16_t)(0) : (uint16_t)(__a[__i] >> __n);
  return __r;
}
static __inline__ uint16x4_t vsra_n_u16(uint16x4_t __a, uint16x4_t __b, const int __n) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (uint16_t)(__a[__i] + (__n == 16 ? (uint16_t)(0) : (uint16_t)(__b[__i] >> __n)));
  return __r;
}
static __inline__ uint16x4_t vshl_u16(uint16x4_t __a, int16x4_t __b) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (int8_t)__b[__i] >= 16 ? 0 : (int8_t)__b[__i] <= -16 ? (uint16_t)(0)
        : (int8_t)__b[__i] >= 0 ? (uint16_t)((uint64_t)__a[__i] << (int8_t)__b[__i])
        : (uint16_t)(__a[__i] >> -(int8_t)__b[__i]);
  return __r;
}
static __inline__ uint16x4_t vsli_n_u16(uint16x4_t __a, uint16x4_t __b, const int __n) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (uint16_t)(((uint64_t)__b[__i] << __n)
        | ((uint64_t)__a[__i] & (((uint64_t)-1 >> 48) >> (16 - __n))));
  return __r;
}
static __inline__ uint16x4_t vsri_n_u16(uint16x4_t __a, uint16x4_t __b, const int __n) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (uint16_t)((((uint64_t)__b[__i] & ((uint64_t)-1 >> 48)) >> __n)
        | ((uint64_t)__a[__i] & ~(((uint64_t)-1 >> 48) >> __n)));
  return __r;
}
static __inline__ uint16x4_t vclz_u16(uint16x4_t __a) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (uint16_t)(__a[__i] == 0 ? 16
        : __builtin_clz((uint32_t)__a[__i] & ((uint64_t)-1 >> 48)) - 16);
  return __r;
}
static __inline__ uint32x2_t vqadd_u32(uint32x2_t __a, uint32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (uint32_t)((int64_t)__a[__i] + (int64_t)__b[__i] > ((int64_t)1 << 32) - 1
        ? ((int64_t)1 << 32) - 1 : (int64_t)__a[__i] + (int64_t)__b[__i] < 0 ? 0
        : (int64_t)__a[__i] + (int64_t)__b[__i]);
  return __r;
}
static __inline__ uint32x2_t vqsub_u32(uint32x2_t __a, uint32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (uint32_t)((int64_t)__a[__i] - (int64_t)__b[__i] > ((int64_t)1 << 32) - 1
        ? ((int64_t)1 << 32) - 1 : (int64_t)__a[__i] - (int64_t)__b[__i] < 0 ? 0
        : (int64_t)__a[__i] - (int64_t)__b[__i]);
  return __r;
}
static __inline__ uint32x2_t vhadd_u32(uint32x2_t __a, uint32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint32_t)(((int64_t)__a[__i] + __b[__i]) >> 1);
  return __r;
}
static __inline__ uint32x2_t vrhadd_u32(uint32x2_t __a, uint32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint32_t)(((int64_t)__a[__i] + __b[__i] + 1) >> 1);
  return __r;
}
static __inline__ uint32x2_t vshl_n_u32(uint32x2_t __a, const int __n) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint32_t)((uint64_t)__a[__i] << __n);
  return __r;
}
static __inline__ uint32x2_t vshr_n_u32(uint32x2_t __a, const int __n) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = __n == 32 ? (uint32_t)(0) : (uint32_t)(__a[__i] >> __n);
  return __r;
}
static __inline__ uint32x2_t vsra_n_u32(uint32x2_t __a, uint32x2_t __b, const int __n) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (uint32_t)(__a[__i] + (__n == 32 ? (uint32_t)(0) : (uint32_t)(__b[__i] >> __n)));
  return __r;
}
static __inline__ uint32x2_t vshl_u32(uint32x2_t __a, int32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (int8_t)__b[__i] >= 32 ? 0 : (int8_t)__b[__i] <= -32 ? (uint32_t)(0)
        : (int8_t)__b[__i] >= 0 ? (uint32_t)((uint64_t)__a[__i] << (int8_t)__b[__i])
        : (uint32_t)(__a[__i] >> -(int8_t)__b[__i]);
  return __r;
}
static __inline__ uint32x2_t vsli_n_u32(uint32x2_t __a, uint32x2_t __b, const int __n) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (uint32_t)(((uint64_t)__b[__i] << __n)
        | ((uint64_t)__a[__i] & (((uint64_t)-1 >> 32) >> (32 - __n))));
  return __r;
}
static __inline__ uint32x2_t vsri_n_u32(uint32x2_t __a, uint32x2_t __b, const int __n) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (uint32_t)((((uint64_t)__b[__i] & ((uint64_t)-1 >> 32)) >> __n)
        | ((uint64_t)__a[__i] & ~(((uint64_t)-1 >> 32) >> __n)));
  return __r;
}
static __inline__ uint32x2_t vclz_u32(uint32x2_t __a) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (uint32_t)(__a[__i] == 0 ? 32
        : __builtin_clz((uint32_t)__a[__i] & ((uint64_t)-1 >> 32)) - 0);
  return __r;
}
static __inline__ uint64x1_t vqadd_u64(uint64x1_t __a, uint64x1_t __b) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++)
    __r[__i] = __a[__i] + __b[__i] < __a[__i] ? UINT64_MAX : __a[__i] + __b[__i];
  return __r;
}
static __inline__ uint64x1_t vqsub_u64(uint64x1_t __a, uint64x1_t __b) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __a[__i] < __b[__i] ? 0 : __a[__i] - __b[__i];
  return __r;
}
static __inline__ uint64x1_t vshl_n_u64(uint64x1_t __a, const int __n) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = (uint64_t)((uint64_t)__a[__i] << __n);
  return __r;
}
static __inline__ uint64x1_t vshr_n_u64(uint64x1_t __a, const int __n) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++)
    __r[__i] = __n == 64 ? (uint64_t)(0) : (uint64_t)(__a[__i] >> __n);
  return __r;
}
static __inline__ uint64x1_t vsra_n_u64(uint64x1_t __a, uint64x1_t __b, const int __n) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++)
    __r[__i] = (uint64_t)(__a[__i] + (__n == 64 ? (uint64_t)(0) : (uint64_t)(__b[__i] >> __n)));
  return __r;
}
static __inline__ uint64x1_t vshl_u64(uint64x1_t __a, int64x1_t __b) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++)
    __r[__i] = (int8_t)__b[__i] >= 64 ? 0 : (int8_t)__b[__i] <= -64 ? (uint64_t)(0)
        : (int8_t)__b[__i] >= 0 ? (uint64_t)((uint64_t)__a[__i] << (int8_t)__b[__i])
        : (uint64_t)(__a[__i] >> -(int8_t)__b[__i]);
  return __r;
}
static __inline__ uint64x1_t vsli_n_u64(uint64x1_t __a, uint64x1_t __b, const int __n) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++)
    __r[__i] = (uint64_t)(((uint64_t)__b[__i] << __n)
        | ((uint64_t)__a[__i] & (((uint64_t)-1 >> 0) >> (64 - __n))));
  return __r;
}
static __inline__ uint64x1_t vsri_n_u64(uint64x1_t __a, uint64x1_t __b, const int __n) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++)
    __r[__i] = (uint64_t)((((uint64_t)__b[__i] & ((uint64_t)-1 >> 0)) >> __n)
        | ((uint64_t)__a[__i] & ~(((uint64_t)-1 >> 0) >> __n)));
  return __r;
}
static __inline__ int8x16_t vaddq_s8(int8x16_t __a, int8x16_t __b) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = (int8_t)(__a[__i] + __b[__i]);
  return __r;
}
static __inline__ int8x16_t vsubq_s8(int8x16_t __a, int8x16_t __b) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = (int8_t)(__a[__i] - __b[__i]);
  return __r;
}
static __inline__ int8x16_t vmulq_s8(int8x16_t __a, int8x16_t __b) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = (int8_t)(__a[__i] * __b[__i]);
  return __r;
}
static __inline__ int8x16_t vmlaq_s8(int8x16_t __a, int8x16_t __b, int8x16_t __c) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = (int8_t)(__a[__i] + __b[__i] * __c[__i]);
  return __r;
}
static __inline__ int8x16_t vmlsq_s8(int8x16_t __a, int8x16_t __b, int8x16_t __c) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = (int8_t)(__a[__i] - __b[__i] * __c[__i]);
  return __r;
}
static __inline__ int8x16_t vmaxq_s8(int8x16_t __a, int8x16_t __b) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __a[__i] > __b[__i] ? __a[__i] : __b[__i];
  return __r;
}
static __inline__ int8x16_t vminq_s8(int8x16_t __a, int8x16_t __b) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __a[__i] < __b[__i] ? __a[__i] : __b[__i];
  return __r;
}
static __inline__ int8x16_t vabdq_s8(int8x16_t __a, int8x16_t __b) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++)
    __r[__i] = (int8_t)(__a[__i] > __b[__i] ? __a[__i] - __b[__i] : __b[__i] - __a[__i]);
  return __r;
}
static __inline__ uint8x16_t vceqq_s8(int8x16_t __a, int8x16_t __b) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __a[__i] == __b[__i] ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ uint8x16_t vceqzq_s8(int8x16_t __a) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __a[__i] == 0 ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ uint8x16_t vcgeq_s8(int8x16_t __a, int8x16_t __b) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __a[__i] >= __b[__i] ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ uint8x16_t vcgezq_s8(int8x16_t __a) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __a[__i] >= 0 ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ uint8x16_t vcgtq_s8(int8x16_t __a, int8x16_t __b) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __a[__i] > __b[__i] ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ uint8x16_t vcgtzq_s8(int8x16_t __a) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __a[__i] > 0 ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ uint8x16_t vcleq_s8(int8x16_t __a, int8x16_t __b) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __a[__i] <= __b[__i] ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ uint8x16_t vclezq_s8(int8x16_t __a) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __a[__i] <= 0 ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ uint8x16_t vcltq_s8(int8x16_t __a, int8x16_t __b) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __a[__i] < __b[__i] ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ uint8x16_t vcltzq_s8(int8x16_t __a) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __a[__i] < 0 ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ int8_t vaddvq_s8(int8x16_t __a) {
  int8_t __r = 0;
  for (int __i = 0; __i < 16; __i++) __r += __a[__i];
  return __r;
}
static __inline__ int8_t vmaxvq_s8(int8x16_t __a) {
  int8_t __r = __a[0];
  for (int __i = 1; __i < 16; __i++) if (__a[__i] > __r) __r = __a[__i];
  return __r;
}
static __inline__ int8_t vminvq_s8(int8x16_t __a) {
  int8_t __r = __a[0];
  for (int __i = 1; __i < 16; __i++) if (__a[__i] < __r) __r = __a[__i];
  return __r;
}
static __inline__ int8x16_t vpaddq_s8(int8x16_t __a, int8x16_t __b) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++)
    __r[__i] = __i < 8 ? (int8_t)(__a[2 * __i] + __a[2 * __i + 1]) : (int8_t)(__b[2 * (__i - 8)]
        + __b[2 * (__i - 8) + 1]);
  return __r;
}
static __inline__ int16x8_t vaddq_s16(int16x8_t __a, int16x8_t __b) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int16_t)(__a[__i] + __b[__i]);
  return __r;
}
static __inline__ int16x8_t vsubq_s16(int16x8_t __a, int16x8_t __b) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int16_t)(__a[__i] - __b[__i]);
  return __r;
}
static __inline__ int16x8_t vmulq_s16(int16x8_t __a, int16x8_t __b) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int16_t)(__a[__i] * __b[__i]);
  return __r;
}
static __inline__ int16x8_t vmulq_n_s16(int16x8_t __a, int16_t __b) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int16_t)(__a[__i] * __b);
  return __r;
}
static __inline__ int16x8_t vmlaq_s16(int16x8_t __a, int16x8_t __b, int16x8_t __c) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int16_t)(__a[__i] + __b[__i] * __c[__i]);
  return __r;
}
static __inline__ int16x8_t vmlsq_s16(int16x8_t __a, int16x8_t __b, int16x8_t __c) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int16_t)(__a[__i] - __b[__i] * __c[__i]);
  return __r;
}
static __inline__ int16x8_t vmlaq_n_s16(int16x8_t __a, int16x8_t __b, int16_t __c) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int16_t)(__a[__i] + __b[__i] * __c);
  return __r;
}
static __inline__ int16x8_t vmaxq_s16(int16x8_t __a, int16x8_t __b) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] > __b[__i] ? __a[__i] : __b[__i];
  return __r;
}
static __inline__ int16x8_t vminq_s16(int16x8_t __a, int16x8_t __b) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] < __b[__i] ? __a[__i] : __b[__i];
  return __r;
}
static __inline__ int16x8_t vabdq_s16(int16x8_t __a, int16x8_t __b) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (int16_t)(__a[__i] > __b[__i] ? __a[__i] - __b[__i] : __b[__i] - __a[__i]);
  return __r;
}
static __inline__ uint16x8_t vceqq_s16(int16x8_t __a, int16x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] == __b[__i] ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ uint16x8_t vceqzq_s16(int16x8_t __a) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] == 0 ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ uint16x8_t vcgeq_s16(int16x8_t __a, int16x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] >= __b[__i] ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ uint16x8_t vcgezq_s16(int16x8_t __a) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] >= 0 ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ uint16x8_t vcgtq_s16(int16x8_t __a, int16x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] > __b[__i] ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ uint16x8_t vcgtzq_s16(int16x8_t __a) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] > 0 ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ uint16x8_t vcleq_s16(int16x8_t __a, int16x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] <= __b[__i] ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ uint16x8_t vclezq_s16(int16x8_t __a) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] <= 0 ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ uint16x8_t vcltq_s16(int16x8_t __a, int16x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] < __b[__i] ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ uint16x8_t vcltzq_s16(int16x8_t __a) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] < 0 ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ int16_t vaddvq_s16(int16x8_t __a) {
  int16_t __r = 0;
  for (int __i = 0; __i < 8; __i++) __r += __a[__i];
  return __r;
}
static __inline__ int16_t vmaxvq_s16(int16x8_t __a) {
  int16_t __r = __a[0];
  for (int __i = 1; __i < 8; __i++) if (__a[__i] > __r) __r = __a[__i];
  return __r;
}
static __inline__ int16_t vminvq_s16(int16x8_t __a) {
  int16_t __r = __a[0];
  for (int __i = 1; __i < 8; __i++) if (__a[__i] < __r) __r = __a[__i];
  return __r;
}
static __inline__ int16x8_t vpaddq_s16(int16x8_t __a, int16x8_t __b) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = __i < 4 ? (int16_t)(__a[2 * __i] + __a[2 * __i + 1])
        : (int16_t)(__b[2 * (__i - 4)] + __b[2 * (__i - 4) + 1]);
  return __r;
}
static __inline__ int32x4_t vaddq_s32(int32x4_t __a, int32x4_t __b) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int32_t)(__a[__i] + __b[__i]);
  return __r;
}
static __inline__ int32x4_t vsubq_s32(int32x4_t __a, int32x4_t __b) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int32_t)(__a[__i] - __b[__i]);
  return __r;
}
static __inline__ int32x4_t vmulq_s32(int32x4_t __a, int32x4_t __b) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int32_t)(__a[__i] * __b[__i]);
  return __r;
}
static __inline__ int32x4_t vmulq_n_s32(int32x4_t __a, int32_t __b) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int32_t)(__a[__i] * __b);
  return __r;
}
static __inline__ int32x4_t vmlaq_s32(int32x4_t __a, int32x4_t __b, int32x4_t __c) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int32_t)(__a[__i] + __b[__i] * __c[__i]);
  return __r;
}
static __inline__ int32x4_t vmlsq_s32(int32x4_t __a, int32x4_t __b, int32x4_t __c) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int32_t)(__a[__i] - __b[__i] * __c[__i]);
  return __r;
}
static __inline__ int32x4_t vmlaq_n_s32(int32x4_t __a, int32x4_t __b, int32_t __c) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int32_t)(__a[__i] + __b[__i] * __c);
  return __r;
}
static __inline__ int32x4_t vmaxq_s32(int32x4_t __a, int32x4_t __b) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] > __b[__i] ? __a[__i] : __b[__i];
  return __r;
}
static __inline__ int32x4_t vminq_s32(int32x4_t __a, int32x4_t __b) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] < __b[__i] ? __a[__i] : __b[__i];
  return __r;
}
static __inline__ int32x4_t vabdq_s32(int32x4_t __a, int32x4_t __b) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (int32_t)(__a[__i] > __b[__i] ? __a[__i] - __b[__i] : __b[__i] - __a[__i]);
  return __r;
}
static __inline__ uint32x4_t vceqq_s32(int32x4_t __a, int32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] == __b[__i] ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x4_t vceqzq_s32(int32x4_t __a) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] == 0 ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x4_t vcgeq_s32(int32x4_t __a, int32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] >= __b[__i] ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x4_t vcgezq_s32(int32x4_t __a) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] >= 0 ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x4_t vcgtq_s32(int32x4_t __a, int32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] > __b[__i] ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x4_t vcgtzq_s32(int32x4_t __a) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] > 0 ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x4_t vcleq_s32(int32x4_t __a, int32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] <= __b[__i] ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x4_t vclezq_s32(int32x4_t __a) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] <= 0 ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x4_t vcltq_s32(int32x4_t __a, int32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] < __b[__i] ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x4_t vcltzq_s32(int32x4_t __a) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] < 0 ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ int32_t vaddvq_s32(int32x4_t __a) {
  int32_t __r = 0;
  for (int __i = 0; __i < 4; __i++) __r += __a[__i];
  return __r;
}
static __inline__ int32_t vmaxvq_s32(int32x4_t __a) {
  int32_t __r = __a[0];
  for (int __i = 1; __i < 4; __i++) if (__a[__i] > __r) __r = __a[__i];
  return __r;
}
static __inline__ int32_t vminvq_s32(int32x4_t __a) {
  int32_t __r = __a[0];
  for (int __i = 1; __i < 4; __i++) if (__a[__i] < __r) __r = __a[__i];
  return __r;
}
static __inline__ int32x4_t vpaddq_s32(int32x4_t __a, int32x4_t __b) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = __i < 2 ? (int32_t)(__a[2 * __i] + __a[2 * __i + 1])
        : (int32_t)(__b[2 * (__i - 2)] + __b[2 * (__i - 2) + 1]);
  return __r;
}
static __inline__ int64x2_t vaddq_s64(int64x2_t __a, int64x2_t __b) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int64_t)(__a[__i] + __b[__i]);
  return __r;
}
static __inline__ int64x2_t vsubq_s64(int64x2_t __a, int64x2_t __b) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int64_t)(__a[__i] - __b[__i]);
  return __r;
}
static __inline__ uint64x2_t vceqq_s64(int64x2_t __a, int64x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] == __b[__i] ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x2_t vceqzq_s64(int64x2_t __a) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] == 0 ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x2_t vcgeq_s64(int64x2_t __a, int64x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] >= __b[__i] ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x2_t vcgezq_s64(int64x2_t __a) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] >= 0 ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x2_t vcgtq_s64(int64x2_t __a, int64x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] > __b[__i] ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x2_t vcgtzq_s64(int64x2_t __a) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] > 0 ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x2_t vcleq_s64(int64x2_t __a, int64x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] <= __b[__i] ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x2_t vclezq_s64(int64x2_t __a) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] <= 0 ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x2_t vcltq_s64(int64x2_t __a, int64x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] < __b[__i] ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x2_t vcltzq_s64(int64x2_t __a) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] < 0 ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ int64_t vaddvq_s64(int64x2_t __a) {
  int64_t __r = 0;
  for (int __i = 0; __i < 2; __i++) __r += __a[__i];
  return __r;
}
static __inline__ int64x2_t vpaddq_s64(int64x2_t __a, int64x2_t __b) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = __i < 1 ? (int64_t)(__a[2 * __i] + __a[2 * __i + 1])
        : (int64_t)(__b[2 * (__i - 1)] + __b[2 * (__i - 1) + 1]);
  return __r;
}
static __inline__ uint8x16_t vaddq_u8(uint8x16_t __a, uint8x16_t __b) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = (uint8_t)(__a[__i] + __b[__i]);
  return __r;
}
static __inline__ uint8x16_t vsubq_u8(uint8x16_t __a, uint8x16_t __b) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = (uint8_t)(__a[__i] - __b[__i]);
  return __r;
}
static __inline__ uint8x16_t vmulq_u8(uint8x16_t __a, uint8x16_t __b) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = (uint8_t)(__a[__i] * __b[__i]);
  return __r;
}
static __inline__ uint8x16_t vmlaq_u8(uint8x16_t __a, uint8x16_t __b, uint8x16_t __c) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = (uint8_t)(__a[__i] + __b[__i] * __c[__i]);
  return __r;
}
static __inline__ uint8x16_t vmlsq_u8(uint8x16_t __a, uint8x16_t __b, uint8x16_t __c) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = (uint8_t)(__a[__i] - __b[__i] * __c[__i]);
  return __r;
}
static __inline__ uint8x16_t vmaxq_u8(uint8x16_t __a, uint8x16_t __b) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __a[__i] > __b[__i] ? __a[__i] : __b[__i];
  return __r;
}
static __inline__ uint8x16_t vminq_u8(uint8x16_t __a, uint8x16_t __b) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __a[__i] < __b[__i] ? __a[__i] : __b[__i];
  return __r;
}
static __inline__ uint8x16_t vabdq_u8(uint8x16_t __a, uint8x16_t __b) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++)
    __r[__i] = (uint8_t)(__a[__i] > __b[__i] ? __a[__i] - __b[__i] : __b[__i] - __a[__i]);
  return __r;
}
static __inline__ uint8x16_t vceqq_u8(uint8x16_t __a, uint8x16_t __b) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __a[__i] == __b[__i] ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ uint8x16_t vceqzq_u8(uint8x16_t __a) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __a[__i] == 0 ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ uint8x16_t vcgeq_u8(uint8x16_t __a, uint8x16_t __b) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __a[__i] >= __b[__i] ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ uint8x16_t vcgtq_u8(uint8x16_t __a, uint8x16_t __b) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __a[__i] > __b[__i] ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ uint8x16_t vcleq_u8(uint8x16_t __a, uint8x16_t __b) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __a[__i] <= __b[__i] ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ uint8x16_t vcltq_u8(uint8x16_t __a, uint8x16_t __b) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __a[__i] < __b[__i] ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ uint8_t vaddvq_u8(uint8x16_t __a) {
  uint8_t __r = 0;
  for (int __i = 0; __i < 16; __i++) __r += __a[__i];
  return __r;
}
static __inline__ uint8_t vmaxvq_u8(uint8x16_t __a) {
  uint8_t __r = __a[0];
  for (int __i = 1; __i < 16; __i++) if (__a[__i] > __r) __r = __a[__i];
  return __r;
}
static __inline__ uint8_t vminvq_u8(uint8x16_t __a) {
  uint8_t __r = __a[0];
  for (int __i = 1; __i < 16; __i++) if (__a[__i] < __r) __r = __a[__i];
  return __r;
}
static __inline__ uint8x16_t vpaddq_u8(uint8x16_t __a, uint8x16_t __b) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++)
    __r[__i] = __i < 8 ? (uint8_t)(__a[2 * __i] + __a[2 * __i + 1])
        : (uint8_t)(__b[2 * (__i - 8)] + __b[2 * (__i - 8) + 1]);
  return __r;
}
static __inline__ uint16x8_t vaddq_u16(uint16x8_t __a, uint16x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint16_t)(__a[__i] + __b[__i]);
  return __r;
}
static __inline__ uint16x8_t vsubq_u16(uint16x8_t __a, uint16x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint16_t)(__a[__i] - __b[__i]);
  return __r;
}
static __inline__ uint16x8_t vmulq_u16(uint16x8_t __a, uint16x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint16_t)(__a[__i] * __b[__i]);
  return __r;
}
static __inline__ uint16x8_t vmulq_n_u16(uint16x8_t __a, uint16_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint16_t)(__a[__i] * __b);
  return __r;
}
static __inline__ uint16x8_t vmlaq_u16(uint16x8_t __a, uint16x8_t __b, uint16x8_t __c) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint16_t)(__a[__i] + __b[__i] * __c[__i]);
  return __r;
}
static __inline__ uint16x8_t vmlsq_u16(uint16x8_t __a, uint16x8_t __b, uint16x8_t __c) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint16_t)(__a[__i] - __b[__i] * __c[__i]);
  return __r;
}
static __inline__ uint16x8_t vmlaq_n_u16(uint16x8_t __a, uint16x8_t __b, uint16_t __c) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint16_t)(__a[__i] + __b[__i] * __c);
  return __r;
}
static __inline__ uint16x8_t vmaxq_u16(uint16x8_t __a, uint16x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] > __b[__i] ? __a[__i] : __b[__i];
  return __r;
}
static __inline__ uint16x8_t vminq_u16(uint16x8_t __a, uint16x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] < __b[__i] ? __a[__i] : __b[__i];
  return __r;
}
static __inline__ uint16x8_t vabdq_u16(uint16x8_t __a, uint16x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (uint16_t)(__a[__i] > __b[__i] ? __a[__i] - __b[__i] : __b[__i] - __a[__i]);
  return __r;
}
static __inline__ uint16x8_t vceqq_u16(uint16x8_t __a, uint16x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] == __b[__i] ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ uint16x8_t vceqzq_u16(uint16x8_t __a) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] == 0 ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ uint16x8_t vcgeq_u16(uint16x8_t __a, uint16x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] >= __b[__i] ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ uint16x8_t vcgtq_u16(uint16x8_t __a, uint16x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] > __b[__i] ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ uint16x8_t vcleq_u16(uint16x8_t __a, uint16x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] <= __b[__i] ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ uint16x8_t vcltq_u16(uint16x8_t __a, uint16x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] < __b[__i] ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ uint16_t vaddvq_u16(uint16x8_t __a) {
  uint16_t __r = 0;
  for (int __i = 0; __i < 8; __i++) __r += __a[__i];
  return __r;
}
static __inline__ uint16_t vmaxvq_u16(uint16x8_t __a) {
  uint16_t __r = __a[0];
  for (int __i = 1; __i < 8; __i++) if (__a[__i] > __r) __r = __a[__i];
  return __r;
}
static __inline__ uint16_t vminvq_u16(uint16x8_t __a) {
  uint16_t __r = __a[0];
  for (int __i = 1; __i < 8; __i++) if (__a[__i] < __r) __r = __a[__i];
  return __r;
}
static __inline__ uint16x8_t vpaddq_u16(uint16x8_t __a, uint16x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = __i < 4 ? (uint16_t)(__a[2 * __i] + __a[2 * __i + 1])
        : (uint16_t)(__b[2 * (__i - 4)] + __b[2 * (__i - 4) + 1]);
  return __r;
}
static __inline__ uint32x4_t vaddq_u32(uint32x4_t __a, uint32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint32_t)(__a[__i] + __b[__i]);
  return __r;
}
static __inline__ uint32x4_t vsubq_u32(uint32x4_t __a, uint32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint32_t)(__a[__i] - __b[__i]);
  return __r;
}
static __inline__ uint32x4_t vmulq_u32(uint32x4_t __a, uint32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint32_t)(__a[__i] * __b[__i]);
  return __r;
}
static __inline__ uint32x4_t vmulq_n_u32(uint32x4_t __a, uint32_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint32_t)(__a[__i] * __b);
  return __r;
}
static __inline__ uint32x4_t vmlaq_u32(uint32x4_t __a, uint32x4_t __b, uint32x4_t __c) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint32_t)(__a[__i] + __b[__i] * __c[__i]);
  return __r;
}
static __inline__ uint32x4_t vmlsq_u32(uint32x4_t __a, uint32x4_t __b, uint32x4_t __c) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint32_t)(__a[__i] - __b[__i] * __c[__i]);
  return __r;
}
static __inline__ uint32x4_t vmlaq_n_u32(uint32x4_t __a, uint32x4_t __b, uint32_t __c) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint32_t)(__a[__i] + __b[__i] * __c);
  return __r;
}
static __inline__ uint32x4_t vmaxq_u32(uint32x4_t __a, uint32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] > __b[__i] ? __a[__i] : __b[__i];
  return __r;
}
static __inline__ uint32x4_t vminq_u32(uint32x4_t __a, uint32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] < __b[__i] ? __a[__i] : __b[__i];
  return __r;
}
static __inline__ uint32x4_t vabdq_u32(uint32x4_t __a, uint32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (uint32_t)(__a[__i] > __b[__i] ? __a[__i] - __b[__i] : __b[__i] - __a[__i]);
  return __r;
}
static __inline__ uint32x4_t vceqq_u32(uint32x4_t __a, uint32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] == __b[__i] ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x4_t vceqzq_u32(uint32x4_t __a) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] == 0 ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x4_t vcgeq_u32(uint32x4_t __a, uint32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] >= __b[__i] ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x4_t vcgtq_u32(uint32x4_t __a, uint32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] > __b[__i] ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x4_t vcleq_u32(uint32x4_t __a, uint32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] <= __b[__i] ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x4_t vcltq_u32(uint32x4_t __a, uint32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] < __b[__i] ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32_t vaddvq_u32(uint32x4_t __a) {
  uint32_t __r = 0;
  for (int __i = 0; __i < 4; __i++) __r += __a[__i];
  return __r;
}
static __inline__ uint32_t vmaxvq_u32(uint32x4_t __a) {
  uint32_t __r = __a[0];
  for (int __i = 1; __i < 4; __i++) if (__a[__i] > __r) __r = __a[__i];
  return __r;
}
static __inline__ uint32_t vminvq_u32(uint32x4_t __a) {
  uint32_t __r = __a[0];
  for (int __i = 1; __i < 4; __i++) if (__a[__i] < __r) __r = __a[__i];
  return __r;
}
static __inline__ uint32x4_t vpaddq_u32(uint32x4_t __a, uint32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = __i < 2 ? (uint32_t)(__a[2 * __i] + __a[2 * __i + 1])
        : (uint32_t)(__b[2 * (__i - 2)] + __b[2 * (__i - 2) + 1]);
  return __r;
}
static __inline__ uint64x2_t vaddq_u64(uint64x2_t __a, uint64x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint64_t)(__a[__i] + __b[__i]);
  return __r;
}
static __inline__ uint64x2_t vsubq_u64(uint64x2_t __a, uint64x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint64_t)(__a[__i] - __b[__i]);
  return __r;
}
static __inline__ uint64x2_t vceqq_u64(uint64x2_t __a, uint64x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] == __b[__i] ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x2_t vceqzq_u64(uint64x2_t __a) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] == 0 ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x2_t vcgeq_u64(uint64x2_t __a, uint64x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] >= __b[__i] ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x2_t vcgtq_u64(uint64x2_t __a, uint64x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] > __b[__i] ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x2_t vcleq_u64(uint64x2_t __a, uint64x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] <= __b[__i] ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x2_t vcltq_u64(uint64x2_t __a, uint64x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] < __b[__i] ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64_t vaddvq_u64(uint64x2_t __a) {
  uint64_t __r = 0;
  for (int __i = 0; __i < 2; __i++) __r += __a[__i];
  return __r;
}
static __inline__ uint64x2_t vpaddq_u64(uint64x2_t __a, uint64x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = __i < 1 ? (uint64_t)(__a[2 * __i] + __a[2 * __i + 1])
        : (uint64_t)(__b[2 * (__i - 1)] + __b[2 * (__i - 1) + 1]);
  return __r;
}
static __inline__ float32x4_t vaddq_f32(float32x4_t __a, float32x4_t __b) {
  float32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (float32_t)(__a[__i] + __b[__i]);
  return __r;
}
static __inline__ float32x4_t vsubq_f32(float32x4_t __a, float32x4_t __b) {
  float32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (float32_t)(__a[__i] - __b[__i]);
  return __r;
}
static __inline__ float32x4_t vmulq_f32(float32x4_t __a, float32x4_t __b) {
  float32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (float32_t)(__a[__i] * __b[__i]);
  return __r;
}
static __inline__ float32x4_t vmulq_n_f32(float32x4_t __a, float32_t __b) {
  float32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (float32_t)(__a[__i] * __b);
  return __r;
}
static __inline__ float32x4_t vmlaq_f32(float32x4_t __a, float32x4_t __b, float32x4_t __c) {
  float32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (float32_t)(__a[__i] + __b[__i] * __c[__i]);
  return __r;
}
static __inline__ float32x4_t vmlsq_f32(float32x4_t __a, float32x4_t __b, float32x4_t __c) {
  float32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (float32_t)(__a[__i] - __b[__i] * __c[__i]);
  return __r;
}
static __inline__ float32x4_t vmlaq_n_f32(float32x4_t __a, float32x4_t __b, float32_t __c) {
  float32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (float32_t)(__a[__i] + __b[__i] * __c);
  return __r;
}
static __inline__ float32x4_t vmaxq_f32(float32x4_t __a, float32x4_t __b) {
  float32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] > __b[__i] ? __a[__i] : __b[__i];
  return __r;
}
static __inline__ float32x4_t vminq_f32(float32x4_t __a, float32x4_t __b) {
  float32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] < __b[__i] ? __a[__i] : __b[__i];
  return __r;
}
static __inline__ float32x4_t vabdq_f32(float32x4_t __a, float32x4_t __b) {
  float32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (float32_t)(__a[__i] > __b[__i] ? __a[__i] - __b[__i] : __b[__i] - __a[__i]);
  return __r;
}
static __inline__ uint32x4_t vceqq_f32(float32x4_t __a, float32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] == __b[__i] ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x4_t vceqzq_f32(float32x4_t __a) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] == 0 ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x4_t vcgeq_f32(float32x4_t __a, float32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] >= __b[__i] ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x4_t vcgezq_f32(float32x4_t __a) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] >= 0 ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x4_t vcgtq_f32(float32x4_t __a, float32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] > __b[__i] ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x4_t vcgtzq_f32(float32x4_t __a) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] > 0 ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x4_t vcleq_f32(float32x4_t __a, float32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] <= __b[__i] ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x4_t vclezq_f32(float32x4_t __a) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] <= 0 ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x4_t vcltq_f32(float32x4_t __a, float32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] < __b[__i] ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint32x4_t vcltzq_f32(float32x4_t __a) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] < 0 ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ float32_t vaddvq_f32(float32x4_t __a) {
  float32_t __r = 0;
  for (int __i = 0; __i < 4; __i++) __r += __a[__i];
  return __r;
}
static __inline__ float32_t vmaxvq_f32(float32x4_t __a) {
  float32_t __r = __a[0];
  for (int __i = 1; __i < 4; __i++) if (__a[__i] > __r) __r = __a[__i];
  return __r;
}
static __inline__ float32_t vminvq_f32(float32x4_t __a) {
  float32_t __r = __a[0];
  for (int __i = 1; __i < 4; __i++) if (__a[__i] < __r) __r = __a[__i];
  return __r;
}
static __inline__ float32x4_t vpaddq_f32(float32x4_t __a, float32x4_t __b) {
  float32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = __i < 2 ? (float32_t)(__a[2 * __i] + __a[2 * __i + 1])
        : (float32_t)(__b[2 * (__i - 2)] + __b[2 * (__i - 2) + 1]);
  return __r;
}
static __inline__ float64x2_t vaddq_f64(float64x2_t __a, float64x2_t __b) {
  float64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (float64_t)(__a[__i] + __b[__i]);
  return __r;
}
static __inline__ float64x2_t vsubq_f64(float64x2_t __a, float64x2_t __b) {
  float64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (float64_t)(__a[__i] - __b[__i]);
  return __r;
}
static __inline__ float64x2_t vmulq_f64(float64x2_t __a, float64x2_t __b) {
  float64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (float64_t)(__a[__i] * __b[__i]);
  return __r;
}
static __inline__ float64x2_t vmulq_n_f64(float64x2_t __a, float64_t __b) {
  float64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (float64_t)(__a[__i] * __b);
  return __r;
}
static __inline__ float64x2_t vmlaq_f64(float64x2_t __a, float64x2_t __b, float64x2_t __c) {
  float64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (float64_t)(__a[__i] + __b[__i] * __c[__i]);
  return __r;
}
static __inline__ float64x2_t vmlsq_f64(float64x2_t __a, float64x2_t __b, float64x2_t __c) {
  float64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (float64_t)(__a[__i] - __b[__i] * __c[__i]);
  return __r;
}
static __inline__ float64x2_t vmaxq_f64(float64x2_t __a, float64x2_t __b) {
  float64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] > __b[__i] ? __a[__i] : __b[__i];
  return __r;
}
static __inline__ float64x2_t vminq_f64(float64x2_t __a, float64x2_t __b) {
  float64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] < __b[__i] ? __a[__i] : __b[__i];
  return __r;
}
static __inline__ float64x2_t vabdq_f64(float64x2_t __a, float64x2_t __b) {
  float64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (float64_t)(__a[__i] > __b[__i] ? __a[__i] - __b[__i] : __b[__i] - __a[__i]);
  return __r;
}
static __inline__ uint64x2_t vceqq_f64(float64x2_t __a, float64x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] == __b[__i] ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x2_t vceqzq_f64(float64x2_t __a) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] == 0 ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x2_t vcgeq_f64(float64x2_t __a, float64x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] >= __b[__i] ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x2_t vcgezq_f64(float64x2_t __a) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] >= 0 ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x2_t vcgtq_f64(float64x2_t __a, float64x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] > __b[__i] ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x2_t vcgtzq_f64(float64x2_t __a) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] > 0 ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x2_t vcleq_f64(float64x2_t __a, float64x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] <= __b[__i] ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x2_t vclezq_f64(float64x2_t __a) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] <= 0 ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x2_t vcltq_f64(float64x2_t __a, float64x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] < __b[__i] ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint64x2_t vcltzq_f64(float64x2_t __a) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] < 0 ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ float64_t vaddvq_f64(float64x2_t __a) {
  float64_t __r = 0;
  for (int __i = 0; __i < 2; __i++) __r += __a[__i];
  return __r;
}
static __inline__ float64_t vmaxvq_f64(float64x2_t __a) {
  float64_t __r = __a[0];
  for (int __i = 1; __i < 2; __i++) if (__a[__i] > __r) __r = __a[__i];
  return __r;
}
static __inline__ float64_t vminvq_f64(float64x2_t __a) {
  float64_t __r = __a[0];
  for (int __i = 1; __i < 2; __i++) if (__a[__i] < __r) __r = __a[__i];
  return __r;
}
static __inline__ float64x2_t vpaddq_f64(float64x2_t __a, float64x2_t __b) {
  float64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = __i < 1 ? (float64_t)(__a[2 * __i] + __a[2 * __i + 1])
        : (float64_t)(__b[2 * (__i - 1)] + __b[2 * (__i - 1) + 1]);
  return __r;
}
static __inline__ int8x16_t vnegq_s8(int8x16_t __a) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = (int8_t)-__a[__i];
  return __r;
}
static __inline__ int8x16_t vabsq_s8(int8x16_t __a) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __a[__i] < 0 ? (int8_t)-__a[__i] : __a[__i];
  return __r;
}
static __inline__ int16x8_t vnegq_s16(int16x8_t __a) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int16_t)-__a[__i];
  return __r;
}
static __inline__ int16x8_t vabsq_s16(int16x8_t __a) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i] < 0 ? (int16_t)-__a[__i] : __a[__i];
  return __r;
}
static __inline__ int32x4_t vnegq_s32(int32x4_t __a) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int32_t)-__a[__i];
  return __r;
}
static __inline__ int32x4_t vabsq_s32(int32x4_t __a) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] < 0 ? (int32_t)-__a[__i] : __a[__i];
  return __r;
}
static __inline__ int64x2_t vnegq_s64(int64x2_t __a) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int64_t)-__a[__i];
  return __r;
}
static __inline__ int64x2_t vabsq_s64(int64x2_t __a) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] < 0 ? (int64_t)-__a[__i] : __a[__i];
  return __r;
}
static __inline__ float32x4_t vnegq_f32(float32x4_t __a) {
  float32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (float32_t)-__a[__i];
  return __r;
}
static __inline__ float32x4_t vabsq_f32(float32x4_t __a) {
  float32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] < 0 ? (float32_t)-__a[__i] : __a[__i];
  return __r;
}
static __inline__ float64x2_t vnegq_f64(float64x2_t __a) {
  float64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (float64_t)-__a[__i];
  return __r;
}
static __inline__ float64x2_t vabsq_f64(float64x2_t __a) {
  float64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] < 0 ? (float64_t)-__a[__i] : __a[__i];
  return __r;
}
static __inline__ float32x4_t vdivq_f32(float32x4_t __a, float32x4_t __b) {
  float32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i] / __b[__i];
  return __r;
}
static __inline__ float64x2_t vdivq_f64(float64x2_t __a, float64x2_t __b) {
  float64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] / __b[__i];
  return __r;
}
static __inline__ int8x16_t vandq_s8(int8x16_t __a, int8x16_t __b) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = (int8_t)(__a[__i] & __b[__i]);
  return __r;
}
static __inline__ int8x16_t vorrq_s8(int8x16_t __a, int8x16_t __b) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = (int8_t)(__a[__i] | __b[__i]);
  return __r;
}
static __inline__ int8x16_t veorq_s8(int8x16_t __a, int8x16_t __b) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = (int8_t)(__a[__i] ^ __b[__i]);
  return __r;
}
static __inline__ int8x16_t vbicq_s8(int8x16_t __a, int8x16_t __b) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = (int8_t)(__a[__i] & ~__b[__i]);
  return __r;
}
static __inline__ int8x16_t vornq_s8(int8x16_t __a, int8x16_t __b) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = (int8_t)(__a[__i] | ~__b[__i]);
  return __r;
}
static __inline__ int8x16_t vmvnq_s8(int8x16_t __a) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = (int8_t)~__a[__i];
  return __r;
}
static __inline__ uint8x16_t vtstq_s8(int8x16_t __a, int8x16_t __b) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = (__a[__i] & __b[__i]) ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ int16x8_t vandq_s16(int16x8_t __a, int16x8_t __b) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int16_t)(__a[__i] & __b[__i]);
  return __r;
}
static __inline__ int16x8_t vorrq_s16(int16x8_t __a, int16x8_t __b) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int16_t)(__a[__i] | __b[__i]);
  return __r;
}
static __inline__ int16x8_t veorq_s16(int16x8_t __a, int16x8_t __b) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int16_t)(__a[__i] ^ __b[__i]);
  return __r;
}
static __inline__ int16x8_t vbicq_s16(int16x8_t __a, int16x8_t __b) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int16_t)(__a[__i] & ~__b[__i]);
  return __r;
}
static __inline__ int16x8_t vornq_s16(int16x8_t __a, int16x8_t __b) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int16_t)(__a[__i] | ~__b[__i]);
  return __r;
}
static __inline__ int16x8_t vmvnq_s16(int16x8_t __a) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int16_t)~__a[__i];
  return __r;
}
static __inline__ uint16x8_t vtstq_s16(int16x8_t __a, int16x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (__a[__i] & __b[__i]) ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ int32x4_t vandq_s32(int32x4_t __a, int32x4_t __b) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int32_t)(__a[__i] & __b[__i]);
  return __r;
}
static __inline__ int32x4_t vorrq_s32(int32x4_t __a, int32x4_t __b) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int32_t)(__a[__i] | __b[__i]);
  return __r;
}
static __inline__ int32x4_t veorq_s32(int32x4_t __a, int32x4_t __b) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int32_t)(__a[__i] ^ __b[__i]);
  return __r;
}
static __inline__ int32x4_t vbicq_s32(int32x4_t __a, int32x4_t __b) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int32_t)(__a[__i] & ~__b[__i]);
  return __r;
}
static __inline__ int32x4_t vornq_s32(int32x4_t __a, int32x4_t __b) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int32_t)(__a[__i] | ~__b[__i]);
  return __r;
}
static __inline__ int32x4_t vmvnq_s32(int32x4_t __a) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int32_t)~__a[__i];
  return __r;
}
static __inline__ uint32x4_t vtstq_s32(int32x4_t __a, int32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (__a[__i] & __b[__i]) ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ int64x2_t vandq_s64(int64x2_t __a, int64x2_t __b) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int64_t)(__a[__i] & __b[__i]);
  return __r;
}
static __inline__ int64x2_t vorrq_s64(int64x2_t __a, int64x2_t __b) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int64_t)(__a[__i] | __b[__i]);
  return __r;
}
static __inline__ int64x2_t veorq_s64(int64x2_t __a, int64x2_t __b) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int64_t)(__a[__i] ^ __b[__i]);
  return __r;
}
static __inline__ int64x2_t vbicq_s64(int64x2_t __a, int64x2_t __b) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int64_t)(__a[__i] & ~__b[__i]);
  return __r;
}
static __inline__ int64x2_t vornq_s64(int64x2_t __a, int64x2_t __b) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int64_t)(__a[__i] | ~__b[__i]);
  return __r;
}
static __inline__ uint64x2_t vtstq_s64(int64x2_t __a, int64x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (__a[__i] & __b[__i]) ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ uint8x16_t vandq_u8(uint8x16_t __a, uint8x16_t __b) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = (uint8_t)(__a[__i] & __b[__i]);
  return __r;
}
static __inline__ uint8x16_t vorrq_u8(uint8x16_t __a, uint8x16_t __b) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = (uint8_t)(__a[__i] | __b[__i]);
  return __r;
}
static __inline__ uint8x16_t veorq_u8(uint8x16_t __a, uint8x16_t __b) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = (uint8_t)(__a[__i] ^ __b[__i]);
  return __r;
}
static __inline__ uint8x16_t vbicq_u8(uint8x16_t __a, uint8x16_t __b) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = (uint8_t)(__a[__i] & ~__b[__i]);
  return __r;
}
static __inline__ uint8x16_t vornq_u8(uint8x16_t __a, uint8x16_t __b) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = (uint8_t)(__a[__i] | ~__b[__i]);
  return __r;
}
static __inline__ uint8x16_t vmvnq_u8(uint8x16_t __a) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = (uint8_t)~__a[__i];
  return __r;
}
static __inline__ uint8x16_t vtstq_u8(uint8x16_t __a, uint8x16_t __b) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = (__a[__i] & __b[__i]) ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ uint16x8_t vandq_u16(uint16x8_t __a, uint16x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint16_t)(__a[__i] & __b[__i]);
  return __r;
}
static __inline__ uint16x8_t vorrq_u16(uint16x8_t __a, uint16x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint16_t)(__a[__i] | __b[__i]);
  return __r;
}
static __inline__ uint16x8_t veorq_u16(uint16x8_t __a, uint16x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint16_t)(__a[__i] ^ __b[__i]);
  return __r;
}
static __inline__ uint16x8_t vbicq_u16(uint16x8_t __a, uint16x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint16_t)(__a[__i] & ~__b[__i]);
  return __r;
}
static __inline__ uint16x8_t vornq_u16(uint16x8_t __a, uint16x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint16_t)(__a[__i] | ~__b[__i]);
  return __r;
}
static __inline__ uint16x8_t vmvnq_u16(uint16x8_t __a) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint16_t)~__a[__i];
  return __r;
}
static __inline__ uint16x8_t vtstq_u16(uint16x8_t __a, uint16x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (__a[__i] & __b[__i]) ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ uint32x4_t vandq_u32(uint32x4_t __a, uint32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint32_t)(__a[__i] & __b[__i]);
  return __r;
}
static __inline__ uint32x4_t vorrq_u32(uint32x4_t __a, uint32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint32_t)(__a[__i] | __b[__i]);
  return __r;
}
static __inline__ uint32x4_t veorq_u32(uint32x4_t __a, uint32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint32_t)(__a[__i] ^ __b[__i]);
  return __r;
}
static __inline__ uint32x4_t vbicq_u32(uint32x4_t __a, uint32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint32_t)(__a[__i] & ~__b[__i]);
  return __r;
}
static __inline__ uint32x4_t vornq_u32(uint32x4_t __a, uint32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint32_t)(__a[__i] | ~__b[__i]);
  return __r;
}
static __inline__ uint32x4_t vmvnq_u32(uint32x4_t __a) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint32_t)~__a[__i];
  return __r;
}
static __inline__ uint32x4_t vtstq_u32(uint32x4_t __a, uint32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (__a[__i] & __b[__i]) ? (uint32_t)-1 : 0;
  return __r;
}
static __inline__ uint64x2_t vandq_u64(uint64x2_t __a, uint64x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint64_t)(__a[__i] & __b[__i]);
  return __r;
}
static __inline__ uint64x2_t vorrq_u64(uint64x2_t __a, uint64x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint64_t)(__a[__i] | __b[__i]);
  return __r;
}
static __inline__ uint64x2_t veorq_u64(uint64x2_t __a, uint64x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint64_t)(__a[__i] ^ __b[__i]);
  return __r;
}
static __inline__ uint64x2_t vbicq_u64(uint64x2_t __a, uint64x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint64_t)(__a[__i] & ~__b[__i]);
  return __r;
}
static __inline__ uint64x2_t vornq_u64(uint64x2_t __a, uint64x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint64_t)(__a[__i] | ~__b[__i]);
  return __r;
}
static __inline__ uint64x2_t vtstq_u64(uint64x2_t __a, uint64x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (__a[__i] & __b[__i]) ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ poly8x16_t vmvnq_p8(poly8x16_t __a) {
  poly8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = (poly8_t)~__a[__i];
  return __r;
}
static __inline__ uint8x16_t vtstq_p8(poly8x16_t __a, poly8x16_t __b) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = (__a[__i] & __b[__i]) ? (uint8_t)-1 : 0;
  return __r;
}
static __inline__ uint16x8_t vtstq_p16(poly16x8_t __a, poly16x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (__a[__i] & __b[__i]) ? (uint16_t)-1 : 0;
  return __r;
}
static __inline__ uint64x2_t vtstq_p64(poly64x2_t __a, poly64x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (__a[__i] & __b[__i]) ? (uint64_t)-1 : 0;
  return __r;
}
static __inline__ int8x16_t vbslq_s8(uint8x16_t __m, int8x16_t __a, int8x16_t __b) {
  uint8x16_t __x, __y;
  __builtin_memcpy(&__x, &__a, sizeof __x);
  __builtin_memcpy(&__y, &__b, sizeof __y);
  __x = (__x & __m) | (__y & ~__m);
  int8x16_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ int16x8_t vbslq_s16(uint16x8_t __m, int16x8_t __a, int16x8_t __b) {
  uint16x8_t __x, __y;
  __builtin_memcpy(&__x, &__a, sizeof __x);
  __builtin_memcpy(&__y, &__b, sizeof __y);
  __x = (__x & __m) | (__y & ~__m);
  int16x8_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ int32x4_t vbslq_s32(uint32x4_t __m, int32x4_t __a, int32x4_t __b) {
  uint32x4_t __x, __y;
  __builtin_memcpy(&__x, &__a, sizeof __x);
  __builtin_memcpy(&__y, &__b, sizeof __y);
  __x = (__x & __m) | (__y & ~__m);
  int32x4_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ int64x2_t vbslq_s64(uint64x2_t __m, int64x2_t __a, int64x2_t __b) {
  uint64x2_t __x, __y;
  __builtin_memcpy(&__x, &__a, sizeof __x);
  __builtin_memcpy(&__y, &__b, sizeof __y);
  __x = (__x & __m) | (__y & ~__m);
  int64x2_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ uint8x16_t vbslq_u8(uint8x16_t __m, uint8x16_t __a, uint8x16_t __b) {
  uint8x16_t __x, __y;
  __builtin_memcpy(&__x, &__a, sizeof __x);
  __builtin_memcpy(&__y, &__b, sizeof __y);
  __x = (__x & __m) | (__y & ~__m);
  uint8x16_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ uint16x8_t vbslq_u16(uint16x8_t __m, uint16x8_t __a, uint16x8_t __b) {
  uint16x8_t __x, __y;
  __builtin_memcpy(&__x, &__a, sizeof __x);
  __builtin_memcpy(&__y, &__b, sizeof __y);
  __x = (__x & __m) | (__y & ~__m);
  uint16x8_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ uint32x4_t vbslq_u32(uint32x4_t __m, uint32x4_t __a, uint32x4_t __b) {
  uint32x4_t __x, __y;
  __builtin_memcpy(&__x, &__a, sizeof __x);
  __builtin_memcpy(&__y, &__b, sizeof __y);
  __x = (__x & __m) | (__y & ~__m);
  uint32x4_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ uint64x2_t vbslq_u64(uint64x2_t __m, uint64x2_t __a, uint64x2_t __b) {
  uint64x2_t __x, __y;
  __builtin_memcpy(&__x, &__a, sizeof __x);
  __builtin_memcpy(&__y, &__b, sizeof __y);
  __x = (__x & __m) | (__y & ~__m);
  uint64x2_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ float32x4_t vbslq_f32(uint32x4_t __m, float32x4_t __a, float32x4_t __b) {
  uint32x4_t __x, __y;
  __builtin_memcpy(&__x, &__a, sizeof __x);
  __builtin_memcpy(&__y, &__b, sizeof __y);
  __x = (__x & __m) | (__y & ~__m);
  float32x4_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ float64x2_t vbslq_f64(uint64x2_t __m, float64x2_t __a, float64x2_t __b) {
  uint64x2_t __x, __y;
  __builtin_memcpy(&__x, &__a, sizeof __x);
  __builtin_memcpy(&__y, &__b, sizeof __y);
  __x = (__x & __m) | (__y & ~__m);
  float64x2_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ poly8x16_t vbslq_p8(uint8x16_t __m, poly8x16_t __a, poly8x16_t __b) {
  uint8x16_t __x, __y;
  __builtin_memcpy(&__x, &__a, sizeof __x);
  __builtin_memcpy(&__y, &__b, sizeof __y);
  __x = (__x & __m) | (__y & ~__m);
  poly8x16_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ poly16x8_t vbslq_p16(uint16x8_t __m, poly16x8_t __a, poly16x8_t __b) {
  uint16x8_t __x, __y;
  __builtin_memcpy(&__x, &__a, sizeof __x);
  __builtin_memcpy(&__y, &__b, sizeof __y);
  __x = (__x & __m) | (__y & ~__m);
  poly16x8_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ poly64x2_t vbslq_p64(uint64x2_t __m, poly64x2_t __a, poly64x2_t __b) {
  uint64x2_t __x, __y;
  __builtin_memcpy(&__x, &__a, sizeof __x);
  __builtin_memcpy(&__y, &__b, sizeof __y);
  __x = (__x & __m) | (__y & ~__m);
  poly64x2_t __r;
  __builtin_memcpy(&__r, &__x, sizeof __r);
  return __r;
}
static __inline__ int8x16_t vqaddq_s8(int8x16_t __a, int8x16_t __b) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++)
    __r[__i] = (int8_t)((int64_t)__a[__i] + (int64_t)__b[__i] > ((int64_t)1 << 7) - 1
        ? ((int64_t)1 << 7) - 1 : (int64_t)__a[__i] + (int64_t)__b[__i] < -((int64_t)1 << 7)
        ? -((int64_t)1 << 7) : (int64_t)__a[__i] + (int64_t)__b[__i]);
  return __r;
}
static __inline__ int8x16_t vqsubq_s8(int8x16_t __a, int8x16_t __b) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++)
    __r[__i] = (int8_t)((int64_t)__a[__i] - (int64_t)__b[__i] > ((int64_t)1 << 7) - 1
        ? ((int64_t)1 << 7) - 1 : (int64_t)__a[__i] - (int64_t)__b[__i] < -((int64_t)1 << 7)
        ? -((int64_t)1 << 7) : (int64_t)__a[__i] - (int64_t)__b[__i]);
  return __r;
}
static __inline__ int8x16_t vhaddq_s8(int8x16_t __a, int8x16_t __b) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = (int8_t)(((int64_t)__a[__i] + __b[__i]) >> 1);
  return __r;
}
static __inline__ int8x16_t vrhaddq_s8(int8x16_t __a, int8x16_t __b) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = (int8_t)(((int64_t)__a[__i] + __b[__i] + 1) >> 1);
  return __r;
}
static __inline__ int8x16_t vshlq_n_s8(int8x16_t __a, const int __n) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = (int8_t)((uint64_t)__a[__i] << __n);
  return __r;
}
static __inline__ int8x16_t vshrq_n_s8(int8x16_t __a, const int __n) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++)
    __r[__i] = __n == 8 ? (int8_t)(__a[__i] < 0 ? -1 : 0) : (int8_t)(__a[__i] >> __n);
  return __r;
}
static __inline__ int8x16_t vsraq_n_s8(int8x16_t __a, int8x16_t __b, const int __n) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++)
    __r[__i] = (int8_t)(__a[__i] + (__n == 8 ? (int8_t)(__b[__i] < 0 ? -1 : 0)
        : (int8_t)(__b[__i] >> __n)));
  return __r;
}
static __inline__ int8x16_t vshlq_s8(int8x16_t __a, int8x16_t __b) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++)
    __r[__i] = (int8_t)__b[__i] >= 8 ? 0 : (int8_t)__b[__i] <= -8 ? (int8_t)(__a[__i] < 0 ? -1
        : 0) : (int8_t)__b[__i] >= 0 ? (int8_t)((uint64_t)__a[__i] << (int8_t)__b[__i])
        : (int8_t)(__a[__i] >> -(int8_t)__b[__i]);
  return __r;
}
static __inline__ int8x16_t vsliq_n_s8(int8x16_t __a, int8x16_t __b, const int __n) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++)
    __r[__i] = (int8_t)(((uint64_t)__b[__i] << __n)
        | ((uint64_t)__a[__i] & (((uint64_t)-1 >> 56) >> (8 - __n))));
  return __r;
}
static __inline__ int8x16_t vsriq_n_s8(int8x16_t __a, int8x16_t __b, const int __n) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++)
    __r[__i] = (int8_t)((((uint64_t)__b[__i] & ((uint64_t)-1 >> 56)) >> __n)
        | ((uint64_t)__a[__i] & ~(((uint64_t)-1 >> 56) >> __n)));
  return __r;
}
static __inline__ int8x16_t vclzq_s8(int8x16_t __a) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++)
    __r[__i] = (int8_t)(__a[__i] == 0 ? 8
        : __builtin_clz((uint32_t)__a[__i] & ((uint64_t)-1 >> 56)) - 24);
  return __r;
}
static __inline__ int8x16_t vcntq_s8(int8x16_t __a) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = (int8_t)__builtin_popcount((uint8_t)__a[__i]);
  return __r;
}
static __inline__ int8x16_t vrbitq_s8(int8x16_t __a) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++)
    __r[__i] = (int8_t)(((((uint8_t)__a[__i] * 0x0802u & 0x22110u)
        | ((uint8_t)__a[__i] * 0x8020u & 0x88440u)) * 0x10101u >> 16) & 0xff);
  return __r;
}
static __inline__ int16x8_t vqaddq_s16(int16x8_t __a, int16x8_t __b) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (int16_t)((int64_t)__a[__i] + (int64_t)__b[__i] > ((int64_t)1 << 15) - 1
        ? ((int64_t)1 << 15) - 1 : (int64_t)__a[__i] + (int64_t)__b[__i] < -((int64_t)1 << 15)
        ? -((int64_t)1 << 15) : (int64_t)__a[__i] + (int64_t)__b[__i]);
  return __r;
}
static __inline__ int16x8_t vqsubq_s16(int16x8_t __a, int16x8_t __b) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (int16_t)((int64_t)__a[__i] - (int64_t)__b[__i] > ((int64_t)1 << 15) - 1
        ? ((int64_t)1 << 15) - 1 : (int64_t)__a[__i] - (int64_t)__b[__i] < -((int64_t)1 << 15)
        ? -((int64_t)1 << 15) : (int64_t)__a[__i] - (int64_t)__b[__i]);
  return __r;
}
static __inline__ int16x8_t vhaddq_s16(int16x8_t __a, int16x8_t __b) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int16_t)(((int64_t)__a[__i] + __b[__i]) >> 1);
  return __r;
}
static __inline__ int16x8_t vrhaddq_s16(int16x8_t __a, int16x8_t __b) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int16_t)(((int64_t)__a[__i] + __b[__i] + 1) >> 1);
  return __r;
}
static __inline__ int16x8_t vshlq_n_s16(int16x8_t __a, const int __n) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int16_t)((uint64_t)__a[__i] << __n);
  return __r;
}
static __inline__ int16x8_t vshrq_n_s16(int16x8_t __a, const int __n) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = __n == 16 ? (int16_t)(__a[__i] < 0 ? -1 : 0) : (int16_t)(__a[__i] >> __n);
  return __r;
}
static __inline__ int16x8_t vsraq_n_s16(int16x8_t __a, int16x8_t __b, const int __n) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (int16_t)(__a[__i] + (__n == 16 ? (int16_t)(__b[__i] < 0 ? -1 : 0)
        : (int16_t)(__b[__i] >> __n)));
  return __r;
}
static __inline__ int16x8_t vshlq_s16(int16x8_t __a, int16x8_t __b) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (int8_t)__b[__i] >= 16 ? 0 : (int8_t)__b[__i] <= -16 ? (int16_t)(__a[__i] < 0 ? -1
        : 0) : (int8_t)__b[__i] >= 0 ? (int16_t)((uint64_t)__a[__i] << (int8_t)__b[__i])
        : (int16_t)(__a[__i] >> -(int8_t)__b[__i]);
  return __r;
}
static __inline__ int16x8_t vsliq_n_s16(int16x8_t __a, int16x8_t __b, const int __n) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (int16_t)(((uint64_t)__b[__i] << __n)
        | ((uint64_t)__a[__i] & (((uint64_t)-1 >> 48) >> (16 - __n))));
  return __r;
}
static __inline__ int16x8_t vsriq_n_s16(int16x8_t __a, int16x8_t __b, const int __n) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (int16_t)((((uint64_t)__b[__i] & ((uint64_t)-1 >> 48)) >> __n)
        | ((uint64_t)__a[__i] & ~(((uint64_t)-1 >> 48) >> __n)));
  return __r;
}
static __inline__ int16x8_t vclzq_s16(int16x8_t __a) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (int16_t)(__a[__i] == 0 ? 16
        : __builtin_clz((uint32_t)__a[__i] & ((uint64_t)-1 >> 48)) - 16);
  return __r;
}
static __inline__ int32x4_t vqaddq_s32(int32x4_t __a, int32x4_t __b) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (int32_t)((int64_t)__a[__i] + (int64_t)__b[__i] > ((int64_t)1 << 31) - 1
        ? ((int64_t)1 << 31) - 1 : (int64_t)__a[__i] + (int64_t)__b[__i] < -((int64_t)1 << 31)
        ? -((int64_t)1 << 31) : (int64_t)__a[__i] + (int64_t)__b[__i]);
  return __r;
}
static __inline__ int32x4_t vqsubq_s32(int32x4_t __a, int32x4_t __b) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (int32_t)((int64_t)__a[__i] - (int64_t)__b[__i] > ((int64_t)1 << 31) - 1
        ? ((int64_t)1 << 31) - 1 : (int64_t)__a[__i] - (int64_t)__b[__i] < -((int64_t)1 << 31)
        ? -((int64_t)1 << 31) : (int64_t)__a[__i] - (int64_t)__b[__i]);
  return __r;
}
static __inline__ int32x4_t vhaddq_s32(int32x4_t __a, int32x4_t __b) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int32_t)(((int64_t)__a[__i] + __b[__i]) >> 1);
  return __r;
}
static __inline__ int32x4_t vrhaddq_s32(int32x4_t __a, int32x4_t __b) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int32_t)(((int64_t)__a[__i] + __b[__i] + 1) >> 1);
  return __r;
}
static __inline__ int32x4_t vshlq_n_s32(int32x4_t __a, const int __n) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int32_t)((uint64_t)__a[__i] << __n);
  return __r;
}
static __inline__ int32x4_t vshrq_n_s32(int32x4_t __a, const int __n) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = __n == 32 ? (int32_t)(__a[__i] < 0 ? -1 : 0) : (int32_t)(__a[__i] >> __n);
  return __r;
}
static __inline__ int32x4_t vsraq_n_s32(int32x4_t __a, int32x4_t __b, const int __n) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (int32_t)(__a[__i] + (__n == 32 ? (int32_t)(__b[__i] < 0 ? -1 : 0)
        : (int32_t)(__b[__i] >> __n)));
  return __r;
}
static __inline__ int32x4_t vshlq_s32(int32x4_t __a, int32x4_t __b) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (int8_t)__b[__i] >= 32 ? 0 : (int8_t)__b[__i] <= -32 ? (int32_t)(__a[__i] < 0 ? -1
        : 0) : (int8_t)__b[__i] >= 0 ? (int32_t)((uint64_t)__a[__i] << (int8_t)__b[__i])
        : (int32_t)(__a[__i] >> -(int8_t)__b[__i]);
  return __r;
}
static __inline__ int32x4_t vsliq_n_s32(int32x4_t __a, int32x4_t __b, const int __n) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (int32_t)(((uint64_t)__b[__i] << __n)
        | ((uint64_t)__a[__i] & (((uint64_t)-1 >> 32) >> (32 - __n))));
  return __r;
}
static __inline__ int32x4_t vsriq_n_s32(int32x4_t __a, int32x4_t __b, const int __n) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (int32_t)((((uint64_t)__b[__i] & ((uint64_t)-1 >> 32)) >> __n)
        | ((uint64_t)__a[__i] & ~(((uint64_t)-1 >> 32) >> __n)));
  return __r;
}
static __inline__ int32x4_t vclzq_s32(int32x4_t __a) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (int32_t)(__a[__i] == 0 ? 32
        : __builtin_clz((uint32_t)__a[__i] & ((uint64_t)-1 >> 32)) - 0);
  return __r;
}
static __inline__ int64x2_t vqaddq_s64(int64x2_t __a, int64x2_t __b) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = __b[__i] > 0 && __a[__i] > INT64_MAX - __b[__i] ? INT64_MAX
        : __b[__i] < 0 && __a[__i] < INT64_MIN - __b[__i] ? INT64_MIN : __a[__i] + __b[__i];
  return __r;
}
static __inline__ int64x2_t vqsubq_s64(int64x2_t __a, int64x2_t __b) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = __b[__i] < 0 && __a[__i] > INT64_MAX + __b[__i] ? INT64_MAX
        : __b[__i] > 0 && __a[__i] < INT64_MIN + __b[__i] ? INT64_MIN : __a[__i] - __b[__i];
  return __r;
}
static __inline__ int64x2_t vshlq_n_s64(int64x2_t __a, const int __n) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int64_t)((uint64_t)__a[__i] << __n);
  return __r;
}
static __inline__ int64x2_t vshrq_n_s64(int64x2_t __a, const int __n) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = __n == 64 ? (int64_t)(__a[__i] < 0 ? -1 : 0) : (int64_t)(__a[__i] >> __n);
  return __r;
}
static __inline__ int64x2_t vsraq_n_s64(int64x2_t __a, int64x2_t __b, const int __n) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (int64_t)(__a[__i] + (__n == 64 ? (int64_t)(__b[__i] < 0 ? -1 : 0)
        : (int64_t)(__b[__i] >> __n)));
  return __r;
}
static __inline__ int64x2_t vshlq_s64(int64x2_t __a, int64x2_t __b) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (int8_t)__b[__i] >= 64 ? 0 : (int8_t)__b[__i] <= -64 ? (int64_t)(__a[__i] < 0 ? -1
        : 0) : (int8_t)__b[__i] >= 0 ? (int64_t)((uint64_t)__a[__i] << (int8_t)__b[__i])
        : (int64_t)(__a[__i] >> -(int8_t)__b[__i]);
  return __r;
}
static __inline__ int64x2_t vsliq_n_s64(int64x2_t __a, int64x2_t __b, const int __n) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (int64_t)(((uint64_t)__b[__i] << __n)
        | ((uint64_t)__a[__i] & (((uint64_t)-1 >> 0) >> (64 - __n))));
  return __r;
}
static __inline__ int64x2_t vsriq_n_s64(int64x2_t __a, int64x2_t __b, const int __n) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (int64_t)((((uint64_t)__b[__i] & ((uint64_t)-1 >> 0)) >> __n)
        | ((uint64_t)__a[__i] & ~(((uint64_t)-1 >> 0) >> __n)));
  return __r;
}
static __inline__ uint8x16_t vqaddq_u8(uint8x16_t __a, uint8x16_t __b) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++)
    __r[__i] = (uint8_t)((int64_t)__a[__i] + (int64_t)__b[__i] > ((int64_t)1 << 8) - 1
        ? ((int64_t)1 << 8) - 1 : (int64_t)__a[__i] + (int64_t)__b[__i] < 0 ? 0
        : (int64_t)__a[__i] + (int64_t)__b[__i]);
  return __r;
}
static __inline__ uint8x16_t vqsubq_u8(uint8x16_t __a, uint8x16_t __b) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++)
    __r[__i] = (uint8_t)((int64_t)__a[__i] - (int64_t)__b[__i] > ((int64_t)1 << 8) - 1
        ? ((int64_t)1 << 8) - 1 : (int64_t)__a[__i] - (int64_t)__b[__i] < 0 ? 0
        : (int64_t)__a[__i] - (int64_t)__b[__i]);
  return __r;
}
static __inline__ uint8x16_t vhaddq_u8(uint8x16_t __a, uint8x16_t __b) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = (uint8_t)(((int64_t)__a[__i] + __b[__i]) >> 1);
  return __r;
}
static __inline__ uint8x16_t vrhaddq_u8(uint8x16_t __a, uint8x16_t __b) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = (uint8_t)(((int64_t)__a[__i] + __b[__i] + 1) >> 1);
  return __r;
}
static __inline__ uint8x16_t vshlq_n_u8(uint8x16_t __a, const int __n) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = (uint8_t)((uint64_t)__a[__i] << __n);
  return __r;
}
static __inline__ uint8x16_t vshrq_n_u8(uint8x16_t __a, const int __n) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++)
    __r[__i] = __n == 8 ? (uint8_t)(0) : (uint8_t)(__a[__i] >> __n);
  return __r;
}
static __inline__ uint8x16_t vsraq_n_u8(uint8x16_t __a, uint8x16_t __b, const int __n) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++)
    __r[__i] = (uint8_t)(__a[__i] + (__n == 8 ? (uint8_t)(0) : (uint8_t)(__b[__i] >> __n)));
  return __r;
}
static __inline__ uint8x16_t vshlq_u8(uint8x16_t __a, int8x16_t __b) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++)
    __r[__i] = (int8_t)__b[__i] >= 8 ? 0 : (int8_t)__b[__i] <= -8 ? (uint8_t)(0)
        : (int8_t)__b[__i] >= 0 ? (uint8_t)((uint64_t)__a[__i] << (int8_t)__b[__i])
        : (uint8_t)(__a[__i] >> -(int8_t)__b[__i]);
  return __r;
}
static __inline__ uint8x16_t vsliq_n_u8(uint8x16_t __a, uint8x16_t __b, const int __n) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++)
    __r[__i] = (uint8_t)(((uint64_t)__b[__i] << __n)
        | ((uint64_t)__a[__i] & (((uint64_t)-1 >> 56) >> (8 - __n))));
  return __r;
}
static __inline__ uint8x16_t vsriq_n_u8(uint8x16_t __a, uint8x16_t __b, const int __n) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++)
    __r[__i] = (uint8_t)((((uint64_t)__b[__i] & ((uint64_t)-1 >> 56)) >> __n)
        | ((uint64_t)__a[__i] & ~(((uint64_t)-1 >> 56) >> __n)));
  return __r;
}
static __inline__ uint8x16_t vclzq_u8(uint8x16_t __a) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++)
    __r[__i] = (uint8_t)(__a[__i] == 0 ? 8
        : __builtin_clz((uint32_t)__a[__i] & ((uint64_t)-1 >> 56)) - 24);
  return __r;
}
static __inline__ uint8x16_t vcntq_u8(uint8x16_t __a) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = (uint8_t)__builtin_popcount((uint8_t)__a[__i]);
  return __r;
}
static __inline__ uint8x16_t vrbitq_u8(uint8x16_t __a) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++)
    __r[__i] = (uint8_t)(((((uint8_t)__a[__i] * 0x0802u & 0x22110u)
        | ((uint8_t)__a[__i] * 0x8020u & 0x88440u)) * 0x10101u >> 16) & 0xff);
  return __r;
}
static __inline__ uint16x8_t vqaddq_u16(uint16x8_t __a, uint16x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (uint16_t)((int64_t)__a[__i] + (int64_t)__b[__i] > ((int64_t)1 << 16) - 1
        ? ((int64_t)1 << 16) - 1 : (int64_t)__a[__i] + (int64_t)__b[__i] < 0 ? 0
        : (int64_t)__a[__i] + (int64_t)__b[__i]);
  return __r;
}
static __inline__ uint16x8_t vqsubq_u16(uint16x8_t __a, uint16x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (uint16_t)((int64_t)__a[__i] - (int64_t)__b[__i] > ((int64_t)1 << 16) - 1
        ? ((int64_t)1 << 16) - 1 : (int64_t)__a[__i] - (int64_t)__b[__i] < 0 ? 0
        : (int64_t)__a[__i] - (int64_t)__b[__i]);
  return __r;
}
static __inline__ uint16x8_t vhaddq_u16(uint16x8_t __a, uint16x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint16_t)(((int64_t)__a[__i] + __b[__i]) >> 1);
  return __r;
}
static __inline__ uint16x8_t vrhaddq_u16(uint16x8_t __a, uint16x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint16_t)(((int64_t)__a[__i] + __b[__i] + 1) >> 1);
  return __r;
}
static __inline__ uint16x8_t vshlq_n_u16(uint16x8_t __a, const int __n) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint16_t)((uint64_t)__a[__i] << __n);
  return __r;
}
static __inline__ uint16x8_t vshrq_n_u16(uint16x8_t __a, const int __n) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = __n == 16 ? (uint16_t)(0) : (uint16_t)(__a[__i] >> __n);
  return __r;
}
static __inline__ uint16x8_t vsraq_n_u16(uint16x8_t __a, uint16x8_t __b, const int __n) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (uint16_t)(__a[__i] + (__n == 16 ? (uint16_t)(0) : (uint16_t)(__b[__i] >> __n)));
  return __r;
}
static __inline__ uint16x8_t vshlq_u16(uint16x8_t __a, int16x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (int8_t)__b[__i] >= 16 ? 0 : (int8_t)__b[__i] <= -16 ? (uint16_t)(0)
        : (int8_t)__b[__i] >= 0 ? (uint16_t)((uint64_t)__a[__i] << (int8_t)__b[__i])
        : (uint16_t)(__a[__i] >> -(int8_t)__b[__i]);
  return __r;
}
static __inline__ uint16x8_t vsliq_n_u16(uint16x8_t __a, uint16x8_t __b, const int __n) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (uint16_t)(((uint64_t)__b[__i] << __n)
        | ((uint64_t)__a[__i] & (((uint64_t)-1 >> 48) >> (16 - __n))));
  return __r;
}
static __inline__ uint16x8_t vsriq_n_u16(uint16x8_t __a, uint16x8_t __b, const int __n) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (uint16_t)((((uint64_t)__b[__i] & ((uint64_t)-1 >> 48)) >> __n)
        | ((uint64_t)__a[__i] & ~(((uint64_t)-1 >> 48) >> __n)));
  return __r;
}
static __inline__ uint16x8_t vclzq_u16(uint16x8_t __a) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (uint16_t)(__a[__i] == 0 ? 16
        : __builtin_clz((uint32_t)__a[__i] & ((uint64_t)-1 >> 48)) - 16);
  return __r;
}
static __inline__ uint32x4_t vqaddq_u32(uint32x4_t __a, uint32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (uint32_t)((int64_t)__a[__i] + (int64_t)__b[__i] > ((int64_t)1 << 32) - 1
        ? ((int64_t)1 << 32) - 1 : (int64_t)__a[__i] + (int64_t)__b[__i] < 0 ? 0
        : (int64_t)__a[__i] + (int64_t)__b[__i]);
  return __r;
}
static __inline__ uint32x4_t vqsubq_u32(uint32x4_t __a, uint32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (uint32_t)((int64_t)__a[__i] - (int64_t)__b[__i] > ((int64_t)1 << 32) - 1
        ? ((int64_t)1 << 32) - 1 : (int64_t)__a[__i] - (int64_t)__b[__i] < 0 ? 0
        : (int64_t)__a[__i] - (int64_t)__b[__i]);
  return __r;
}
static __inline__ uint32x4_t vhaddq_u32(uint32x4_t __a, uint32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint32_t)(((int64_t)__a[__i] + __b[__i]) >> 1);
  return __r;
}
static __inline__ uint32x4_t vrhaddq_u32(uint32x4_t __a, uint32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint32_t)(((int64_t)__a[__i] + __b[__i] + 1) >> 1);
  return __r;
}
static __inline__ uint32x4_t vshlq_n_u32(uint32x4_t __a, const int __n) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint32_t)((uint64_t)__a[__i] << __n);
  return __r;
}
static __inline__ uint32x4_t vshrq_n_u32(uint32x4_t __a, const int __n) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = __n == 32 ? (uint32_t)(0) : (uint32_t)(__a[__i] >> __n);
  return __r;
}
static __inline__ uint32x4_t vsraq_n_u32(uint32x4_t __a, uint32x4_t __b, const int __n) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (uint32_t)(__a[__i] + (__n == 32 ? (uint32_t)(0) : (uint32_t)(__b[__i] >> __n)));
  return __r;
}
static __inline__ uint32x4_t vshlq_u32(uint32x4_t __a, int32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (int8_t)__b[__i] >= 32 ? 0 : (int8_t)__b[__i] <= -32 ? (uint32_t)(0)
        : (int8_t)__b[__i] >= 0 ? (uint32_t)((uint64_t)__a[__i] << (int8_t)__b[__i])
        : (uint32_t)(__a[__i] >> -(int8_t)__b[__i]);
  return __r;
}
static __inline__ uint32x4_t vsliq_n_u32(uint32x4_t __a, uint32x4_t __b, const int __n) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (uint32_t)(((uint64_t)__b[__i] << __n)
        | ((uint64_t)__a[__i] & (((uint64_t)-1 >> 32) >> (32 - __n))));
  return __r;
}
static __inline__ uint32x4_t vsriq_n_u32(uint32x4_t __a, uint32x4_t __b, const int __n) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (uint32_t)((((uint64_t)__b[__i] & ((uint64_t)-1 >> 32)) >> __n)
        | ((uint64_t)__a[__i] & ~(((uint64_t)-1 >> 32) >> __n)));
  return __r;
}
static __inline__ uint32x4_t vclzq_u32(uint32x4_t __a) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (uint32_t)(__a[__i] == 0 ? 32
        : __builtin_clz((uint32_t)__a[__i] & ((uint64_t)-1 >> 32)) - 0);
  return __r;
}
static __inline__ uint64x2_t vqaddq_u64(uint64x2_t __a, uint64x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = __a[__i] + __b[__i] < __a[__i] ? UINT64_MAX : __a[__i] + __b[__i];
  return __r;
}
static __inline__ uint64x2_t vqsubq_u64(uint64x2_t __a, uint64x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i] < __b[__i] ? 0 : __a[__i] - __b[__i];
  return __r;
}
static __inline__ uint64x2_t vshlq_n_u64(uint64x2_t __a, const int __n) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint64_t)((uint64_t)__a[__i] << __n);
  return __r;
}
static __inline__ uint64x2_t vshrq_n_u64(uint64x2_t __a, const int __n) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = __n == 64 ? (uint64_t)(0) : (uint64_t)(__a[__i] >> __n);
  return __r;
}
static __inline__ uint64x2_t vsraq_n_u64(uint64x2_t __a, uint64x2_t __b, const int __n) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (uint64_t)(__a[__i] + (__n == 64 ? (uint64_t)(0) : (uint64_t)(__b[__i] >> __n)));
  return __r;
}
static __inline__ uint64x2_t vshlq_u64(uint64x2_t __a, int64x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (int8_t)__b[__i] >= 64 ? 0 : (int8_t)__b[__i] <= -64 ? (uint64_t)(0)
        : (int8_t)__b[__i] >= 0 ? (uint64_t)((uint64_t)__a[__i] << (int8_t)__b[__i])
        : (uint64_t)(__a[__i] >> -(int8_t)__b[__i]);
  return __r;
}
static __inline__ uint64x2_t vsliq_n_u64(uint64x2_t __a, uint64x2_t __b, const int __n) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (uint64_t)(((uint64_t)__b[__i] << __n)
        | ((uint64_t)__a[__i] & (((uint64_t)-1 >> 0) >> (64 - __n))));
  return __r;
}
static __inline__ uint64x2_t vsriq_n_u64(uint64x2_t __a, uint64x2_t __b, const int __n) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (uint64_t)((((uint64_t)__b[__i] & ((uint64_t)-1 >> 0)) >> __n)
        | ((uint64_t)__a[__i] & ~(((uint64_t)-1 >> 0) >> __n)));
  return __r;
}

/* Widening, narrowing and the long forms. */
static __inline__ int16x8_t vmovl_s8(int8x8_t __a) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int16_t)__a[__i];
  return __r;
}
static __inline__ int16x8_t vmovl_high_s8(int8x16_t __a) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int16_t)__a[__i + 8];
  return __r;
}
static __inline__ int8x8_t vmovn_s16(int16x8_t __a) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int8_t)__a[__i];
  return __r;
}
static __inline__ int8x16_t vmovn_high_s16(int8x8_t __lo, int16x8_t __a) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __i < 8 ? __lo[__i] : (int8_t)__a[__i - 8];
  return __r;
}
static __inline__ int8x8_t vshrn_n_s16(int16x8_t __a, const int __k) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int8_t)(__a[__i] >> __k);
  return __r;
}
static __inline__ int8x8_t vrshrn_n_s16(int16x8_t __a, const int __k) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (int8_t)((__a[__i] + ((int16_t)1 << (__k - 1))) >> __k);
  return __r;
}
static __inline__ int16x8_t vaddl_s8(int8x8_t __a, int8x8_t __b) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int16_t)((int16_t)__a[__i] + (int16_t)__b[__i]);
  return __r;
}
static __inline__ int16x8_t vaddl_high_s8(int8x16_t __a, int8x16_t __b) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (int16_t)((int16_t)__a[__i + 8] + (int16_t)__b[__i + 8]);
  return __r;
}
static __inline__ int16x8_t vsubl_s8(int8x8_t __a, int8x8_t __b) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int16_t)((int16_t)__a[__i] - (int16_t)__b[__i]);
  return __r;
}
static __inline__ int16x8_t vsubl_high_s8(int8x16_t __a, int8x16_t __b) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (int16_t)((int16_t)__a[__i + 8] - (int16_t)__b[__i + 8]);
  return __r;
}
static __inline__ int16x8_t vmull_s8(int8x8_t __a, int8x8_t __b) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int16_t)((int16_t)__a[__i] * (int16_t)__b[__i]);
  return __r;
}
static __inline__ int16x8_t vmull_high_s8(int8x16_t __a, int8x16_t __b) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (int16_t)((int16_t)__a[__i + 8] * (int16_t)__b[__i + 8]);
  return __r;
}
static __inline__ int16x8_t vabdl_s8(int8x8_t __a, int8x8_t __b) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (int16_t)(__a[__i] > __b[__i] ? (int16_t)__a[__i] - __b[__i]
        : (int16_t)__b[__i] - __a[__i]);
  return __r;
}
static __inline__ int16x8_t vaddw_s8(int16x8_t __a, int8x8_t __b) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int16_t)(__a[__i] + (int16_t)__b[__i]);
  return __r;
}
static __inline__ int16x8_t vaddw_high_s8(int16x8_t __a, int8x16_t __b) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int16_t)(__a[__i] + (int16_t)__b[__i + 8]);
  return __r;
}
static __inline__ int16x8_t vsubw_s8(int16x8_t __a, int8x8_t __b) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int16_t)(__a[__i] - (int16_t)__b[__i]);
  return __r;
}
static __inline__ int16x8_t vsubw_high_s8(int16x8_t __a, int8x16_t __b) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int16_t)(__a[__i] - (int16_t)__b[__i + 8]);
  return __r;
}
static __inline__ int16x8_t vmlal_s8(int16x8_t __a, int8x8_t __b, int8x8_t __c) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (int16_t)(__a[__i] + (int16_t)__b[__i] * (int16_t)__c[__i]);
  return __r;
}
static __inline__ int16x8_t vmlal_high_s8(int16x8_t __a, int8x16_t __b, int8x16_t __c) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (int16_t)(__a[__i] + (int16_t)__b[__i + 8] * (int16_t)__c[__i + 8]);
  return __r;
}
static __inline__ int16x8_t vmlsl_s8(int16x8_t __a, int8x8_t __b, int8x8_t __c) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (int16_t)(__a[__i] - (int16_t)__b[__i] * (int16_t)__c[__i]);
  return __r;
}
static __inline__ int16x8_t vmlsl_high_s8(int16x8_t __a, int8x16_t __b, int8x16_t __c) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (int16_t)(__a[__i] - (int16_t)__b[__i + 8] * (int16_t)__c[__i + 8]);
  return __r;
}
static __inline__ int16x4_t vpaddl_s8(int8x8_t __a) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int16_t)((int16_t)__a[2 * __i] + __a[2 * __i + 1]);
  return __r;
}
static __inline__ int16x4_t vpadal_s8(int16x4_t __s, int8x8_t __a) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (int16_t)(__s[__i] + (int16_t)__a[2 * __i] + __a[2 * __i + 1]);
  return __r;
}
static __inline__ int16_t vaddlv_s8(int8x8_t __a) {
  int16_t __r = 0;
  for (int __i = 0; __i < 8; __i++) __r += __a[__i];
  return __r;
}
static __inline__ int16x8_t vpaddlq_s8(int8x16_t __a) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int16_t)((int16_t)__a[2 * __i] + __a[2 * __i + 1]);
  return __r;
}
static __inline__ int16x8_t vpadalq_s8(int16x8_t __s, int8x16_t __a) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (int16_t)(__s[__i] + (int16_t)__a[2 * __i] + __a[2 * __i + 1]);
  return __r;
}
static __inline__ int16_t vaddlvq_s8(int8x16_t __a) {
  int16_t __r = 0;
  for (int __i = 0; __i < 16; __i++) __r += __a[__i];
  return __r;
}
static __inline__ int16x8_t vshll_n_s8(int8x8_t __a, const int __k) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int16_t)((int16_t)__a[__i] << __k);
  return __r;
}
static __inline__ int16x8_t vshll_high_n_s8(int8x16_t __a, const int __k) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (int16_t)((int16_t)__a[__i + 8] << __k);
  return __r;
}
static __inline__ int32x4_t vmovl_s16(int16x4_t __a) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int32_t)__a[__i];
  return __r;
}
static __inline__ int32x4_t vmovl_high_s16(int16x8_t __a) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int32_t)__a[__i + 4];
  return __r;
}
static __inline__ int16x4_t vmovn_s32(int32x4_t __a) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int16_t)__a[__i];
  return __r;
}
static __inline__ int16x8_t vmovn_high_s32(int16x4_t __lo, int32x4_t __a) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i < 4 ? __lo[__i] : (int16_t)__a[__i - 4];
  return __r;
}
static __inline__ int16x4_t vshrn_n_s32(int32x4_t __a, const int __k) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int16_t)(__a[__i] >> __k);
  return __r;
}
static __inline__ int16x4_t vrshrn_n_s32(int32x4_t __a, const int __k) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (int16_t)((__a[__i] + ((int32_t)1 << (__k - 1))) >> __k);
  return __r;
}
static __inline__ int32x4_t vaddl_s16(int16x4_t __a, int16x4_t __b) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int32_t)((int32_t)__a[__i] + (int32_t)__b[__i]);
  return __r;
}
static __inline__ int32x4_t vaddl_high_s16(int16x8_t __a, int16x8_t __b) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (int32_t)((int32_t)__a[__i + 4] + (int32_t)__b[__i + 4]);
  return __r;
}
static __inline__ int32x4_t vsubl_s16(int16x4_t __a, int16x4_t __b) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int32_t)((int32_t)__a[__i] - (int32_t)__b[__i]);
  return __r;
}
static __inline__ int32x4_t vsubl_high_s16(int16x8_t __a, int16x8_t __b) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (int32_t)((int32_t)__a[__i + 4] - (int32_t)__b[__i + 4]);
  return __r;
}
static __inline__ int32x4_t vmull_s16(int16x4_t __a, int16x4_t __b) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int32_t)((int32_t)__a[__i] * (int32_t)__b[__i]);
  return __r;
}
static __inline__ int32x4_t vmull_high_s16(int16x8_t __a, int16x8_t __b) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (int32_t)((int32_t)__a[__i + 4] * (int32_t)__b[__i + 4]);
  return __r;
}
static __inline__ int32x4_t vmull_n_s16(int16x4_t __a, int16_t __b) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int32_t)((int32_t)__a[__i] * (int32_t)__b);
  return __r;
}
static __inline__ int32x4_t vabdl_s16(int16x4_t __a, int16x4_t __b) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (int32_t)(__a[__i] > __b[__i] ? (int32_t)__a[__i] - __b[__i]
        : (int32_t)__b[__i] - __a[__i]);
  return __r;
}
static __inline__ int32x4_t vaddw_s16(int32x4_t __a, int16x4_t __b) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int32_t)(__a[__i] + (int32_t)__b[__i]);
  return __r;
}
static __inline__ int32x4_t vaddw_high_s16(int32x4_t __a, int16x8_t __b) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int32_t)(__a[__i] + (int32_t)__b[__i + 4]);
  return __r;
}
static __inline__ int32x4_t vsubw_s16(int32x4_t __a, int16x4_t __b) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int32_t)(__a[__i] - (int32_t)__b[__i]);
  return __r;
}
static __inline__ int32x4_t vsubw_high_s16(int32x4_t __a, int16x8_t __b) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int32_t)(__a[__i] - (int32_t)__b[__i + 4]);
  return __r;
}
static __inline__ int32x4_t vmlal_s16(int32x4_t __a, int16x4_t __b, int16x4_t __c) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (int32_t)(__a[__i] + (int32_t)__b[__i] * (int32_t)__c[__i]);
  return __r;
}
static __inline__ int32x4_t vmlal_high_s16(int32x4_t __a, int16x8_t __b, int16x8_t __c) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (int32_t)(__a[__i] + (int32_t)__b[__i + 4] * (int32_t)__c[__i + 4]);
  return __r;
}
static __inline__ int32x4_t vmlal_n_s16(int32x4_t __a, int16x4_t __b, int16_t __c) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (int32_t)(__a[__i] + (int32_t)__b[__i] * (int32_t)__c);
  return __r;
}
static __inline__ int32x4_t vmlsl_s16(int32x4_t __a, int16x4_t __b, int16x4_t __c) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (int32_t)(__a[__i] - (int32_t)__b[__i] * (int32_t)__c[__i]);
  return __r;
}
static __inline__ int32x4_t vmlsl_high_s16(int32x4_t __a, int16x8_t __b, int16x8_t __c) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (int32_t)(__a[__i] - (int32_t)__b[__i + 4] * (int32_t)__c[__i + 4]);
  return __r;
}
static __inline__ int32x4_t vmlsl_n_s16(int32x4_t __a, int16x4_t __b, int16_t __c) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (int32_t)(__a[__i] - (int32_t)__b[__i] * (int32_t)__c);
  return __r;
}
static __inline__ int32x2_t vpaddl_s16(int16x4_t __a) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int32_t)((int32_t)__a[2 * __i] + __a[2 * __i + 1]);
  return __r;
}
static __inline__ int32x2_t vpadal_s16(int32x2_t __s, int16x4_t __a) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (int32_t)(__s[__i] + (int32_t)__a[2 * __i] + __a[2 * __i + 1]);
  return __r;
}
static __inline__ int32_t vaddlv_s16(int16x4_t __a) {
  int32_t __r = 0;
  for (int __i = 0; __i < 4; __i++) __r += __a[__i];
  return __r;
}
static __inline__ int32x4_t vpaddlq_s16(int16x8_t __a) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int32_t)((int32_t)__a[2 * __i] + __a[2 * __i + 1]);
  return __r;
}
static __inline__ int32x4_t vpadalq_s16(int32x4_t __s, int16x8_t __a) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (int32_t)(__s[__i] + (int32_t)__a[2 * __i] + __a[2 * __i + 1]);
  return __r;
}
static __inline__ int32_t vaddlvq_s16(int16x8_t __a) {
  int32_t __r = 0;
  for (int __i = 0; __i < 8; __i++) __r += __a[__i];
  return __r;
}
static __inline__ int32x4_t vshll_n_s16(int16x4_t __a, const int __k) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int32_t)((int32_t)__a[__i] << __k);
  return __r;
}
static __inline__ int32x4_t vshll_high_n_s16(int16x8_t __a, const int __k) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (int32_t)((int32_t)__a[__i + 4] << __k);
  return __r;
}
static __inline__ int64x2_t vmovl_s32(int32x2_t __a) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int64_t)__a[__i];
  return __r;
}
static __inline__ int64x2_t vmovl_high_s32(int32x4_t __a) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int64_t)__a[__i + 2];
  return __r;
}
static __inline__ int32x2_t vmovn_s64(int64x2_t __a) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int32_t)__a[__i];
  return __r;
}
static __inline__ int32x4_t vmovn_high_s64(int32x2_t __lo, int64x2_t __a) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i < 2 ? __lo[__i] : (int32_t)__a[__i - 2];
  return __r;
}
static __inline__ int32x2_t vshrn_n_s64(int64x2_t __a, const int __k) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int32_t)(__a[__i] >> __k);
  return __r;
}
static __inline__ int32x2_t vrshrn_n_s64(int64x2_t __a, const int __k) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (int32_t)((__a[__i] + ((int64_t)1 << (__k - 1))) >> __k);
  return __r;
}
static __inline__ int64x2_t vaddl_s32(int32x2_t __a, int32x2_t __b) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int64_t)((int64_t)__a[__i] + (int64_t)__b[__i]);
  return __r;
}
static __inline__ int64x2_t vaddl_high_s32(int32x4_t __a, int32x4_t __b) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (int64_t)((int64_t)__a[__i + 2] + (int64_t)__b[__i + 2]);
  return __r;
}
static __inline__ int64x2_t vsubl_s32(int32x2_t __a, int32x2_t __b) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int64_t)((int64_t)__a[__i] - (int64_t)__b[__i]);
  return __r;
}
static __inline__ int64x2_t vsubl_high_s32(int32x4_t __a, int32x4_t __b) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (int64_t)((int64_t)__a[__i + 2] - (int64_t)__b[__i + 2]);
  return __r;
}
static __inline__ int64x2_t vmull_s32(int32x2_t __a, int32x2_t __b) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int64_t)((int64_t)__a[__i] * (int64_t)__b[__i]);
  return __r;
}
static __inline__ int64x2_t vmull_high_s32(int32x4_t __a, int32x4_t __b) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (int64_t)((int64_t)__a[__i + 2] * (int64_t)__b[__i + 2]);
  return __r;
}
static __inline__ int64x2_t vmull_n_s32(int32x2_t __a, int32_t __b) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int64_t)((int64_t)__a[__i] * (int64_t)__b);
  return __r;
}
static __inline__ int64x2_t vabdl_s32(int32x2_t __a, int32x2_t __b) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (int64_t)(__a[__i] > __b[__i] ? (int64_t)__a[__i] - __b[__i]
        : (int64_t)__b[__i] - __a[__i]);
  return __r;
}
static __inline__ int64x2_t vaddw_s32(int64x2_t __a, int32x2_t __b) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int64_t)(__a[__i] + (int64_t)__b[__i]);
  return __r;
}
static __inline__ int64x2_t vaddw_high_s32(int64x2_t __a, int32x4_t __b) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int64_t)(__a[__i] + (int64_t)__b[__i + 2]);
  return __r;
}
static __inline__ int64x2_t vsubw_s32(int64x2_t __a, int32x2_t __b) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int64_t)(__a[__i] - (int64_t)__b[__i]);
  return __r;
}
static __inline__ int64x2_t vsubw_high_s32(int64x2_t __a, int32x4_t __b) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int64_t)(__a[__i] - (int64_t)__b[__i + 2]);
  return __r;
}
static __inline__ int64x2_t vmlal_s32(int64x2_t __a, int32x2_t __b, int32x2_t __c) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (int64_t)(__a[__i] + (int64_t)__b[__i] * (int64_t)__c[__i]);
  return __r;
}
static __inline__ int64x2_t vmlal_high_s32(int64x2_t __a, int32x4_t __b, int32x4_t __c) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (int64_t)(__a[__i] + (int64_t)__b[__i + 2] * (int64_t)__c[__i + 2]);
  return __r;
}
static __inline__ int64x2_t vmlal_n_s32(int64x2_t __a, int32x2_t __b, int32_t __c) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (int64_t)(__a[__i] + (int64_t)__b[__i] * (int64_t)__c);
  return __r;
}
static __inline__ int64x2_t vmlsl_s32(int64x2_t __a, int32x2_t __b, int32x2_t __c) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (int64_t)(__a[__i] - (int64_t)__b[__i] * (int64_t)__c[__i]);
  return __r;
}
static __inline__ int64x2_t vmlsl_high_s32(int64x2_t __a, int32x4_t __b, int32x4_t __c) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (int64_t)(__a[__i] - (int64_t)__b[__i + 2] * (int64_t)__c[__i + 2]);
  return __r;
}
static __inline__ int64x2_t vmlsl_n_s32(int64x2_t __a, int32x2_t __b, int32_t __c) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (int64_t)(__a[__i] - (int64_t)__b[__i] * (int64_t)__c);
  return __r;
}
static __inline__ int64x1_t vpaddl_s32(int32x2_t __a) {
  int64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = (int64_t)((int64_t)__a[2 * __i] + __a[2 * __i + 1]);
  return __r;
}
static __inline__ int64x1_t vpadal_s32(int64x1_t __s, int32x2_t __a) {
  int64x1_t __r;
  for (int __i = 0; __i < 1; __i++)
    __r[__i] = (int64_t)(__s[__i] + (int64_t)__a[2 * __i] + __a[2 * __i + 1]);
  return __r;
}
static __inline__ int64_t vaddlv_s32(int32x2_t __a) {
  int64_t __r = 0;
  for (int __i = 0; __i < 2; __i++) __r += __a[__i];
  return __r;
}
static __inline__ int64x2_t vpaddlq_s32(int32x4_t __a) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int64_t)((int64_t)__a[2 * __i] + __a[2 * __i + 1]);
  return __r;
}
static __inline__ int64x2_t vpadalq_s32(int64x2_t __s, int32x4_t __a) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (int64_t)(__s[__i] + (int64_t)__a[2 * __i] + __a[2 * __i + 1]);
  return __r;
}
static __inline__ int64_t vaddlvq_s32(int32x4_t __a) {
  int64_t __r = 0;
  for (int __i = 0; __i < 4; __i++) __r += __a[__i];
  return __r;
}
static __inline__ int64x2_t vshll_n_s32(int32x2_t __a, const int __k) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int64_t)((int64_t)__a[__i] << __k);
  return __r;
}
static __inline__ int64x2_t vshll_high_n_s32(int32x4_t __a, const int __k) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (int64_t)((int64_t)__a[__i + 2] << __k);
  return __r;
}
static __inline__ uint16x8_t vmovl_u8(uint8x8_t __a) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint16_t)__a[__i];
  return __r;
}
static __inline__ uint16x8_t vmovl_high_u8(uint8x16_t __a) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint16_t)__a[__i + 8];
  return __r;
}
static __inline__ uint8x8_t vmovn_u16(uint16x8_t __a) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint8_t)__a[__i];
  return __r;
}
static __inline__ uint8x16_t vmovn_high_u16(uint8x8_t __lo, uint16x8_t __a) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __i < 8 ? __lo[__i] : (uint8_t)__a[__i - 8];
  return __r;
}
static __inline__ uint8x8_t vshrn_n_u16(uint16x8_t __a, const int __k) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint8_t)(__a[__i] >> __k);
  return __r;
}
static __inline__ uint8x8_t vrshrn_n_u16(uint16x8_t __a, const int __k) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (uint8_t)((__a[__i] + ((uint16_t)1 << (__k - 1))) >> __k);
  return __r;
}
static __inline__ uint16x8_t vaddl_u8(uint8x8_t __a, uint8x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint16_t)((uint16_t)__a[__i] + (uint16_t)__b[__i]);
  return __r;
}
static __inline__ uint16x8_t vaddl_high_u8(uint8x16_t __a, uint8x16_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (uint16_t)((uint16_t)__a[__i + 8] + (uint16_t)__b[__i + 8]);
  return __r;
}
static __inline__ uint16x8_t vsubl_u8(uint8x8_t __a, uint8x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint16_t)((uint16_t)__a[__i] - (uint16_t)__b[__i]);
  return __r;
}
static __inline__ uint16x8_t vsubl_high_u8(uint8x16_t __a, uint8x16_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (uint16_t)((uint16_t)__a[__i + 8] - (uint16_t)__b[__i + 8]);
  return __r;
}
static __inline__ uint16x8_t vmull_u8(uint8x8_t __a, uint8x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint16_t)((uint16_t)__a[__i] * (uint16_t)__b[__i]);
  return __r;
}
static __inline__ uint16x8_t vmull_high_u8(uint8x16_t __a, uint8x16_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (uint16_t)((uint16_t)__a[__i + 8] * (uint16_t)__b[__i + 8]);
  return __r;
}
static __inline__ uint16x8_t vabdl_u8(uint8x8_t __a, uint8x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (uint16_t)(__a[__i] > __b[__i] ? (uint16_t)__a[__i] - __b[__i]
        : (uint16_t)__b[__i] - __a[__i]);
  return __r;
}
static __inline__ uint16x8_t vaddw_u8(uint16x8_t __a, uint8x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint16_t)(__a[__i] + (uint16_t)__b[__i]);
  return __r;
}
static __inline__ uint16x8_t vaddw_high_u8(uint16x8_t __a, uint8x16_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint16_t)(__a[__i] + (uint16_t)__b[__i + 8]);
  return __r;
}
static __inline__ uint16x8_t vsubw_u8(uint16x8_t __a, uint8x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint16_t)(__a[__i] - (uint16_t)__b[__i]);
  return __r;
}
static __inline__ uint16x8_t vsubw_high_u8(uint16x8_t __a, uint8x16_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint16_t)(__a[__i] - (uint16_t)__b[__i + 8]);
  return __r;
}
static __inline__ uint16x8_t vmlal_u8(uint16x8_t __a, uint8x8_t __b, uint8x8_t __c) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (uint16_t)(__a[__i] + (uint16_t)__b[__i] * (uint16_t)__c[__i]);
  return __r;
}
static __inline__ uint16x8_t vmlal_high_u8(uint16x8_t __a, uint8x16_t __b, uint8x16_t __c) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (uint16_t)(__a[__i] + (uint16_t)__b[__i + 8] * (uint16_t)__c[__i + 8]);
  return __r;
}
static __inline__ uint16x8_t vmlsl_u8(uint16x8_t __a, uint8x8_t __b, uint8x8_t __c) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (uint16_t)(__a[__i] - (uint16_t)__b[__i] * (uint16_t)__c[__i]);
  return __r;
}
static __inline__ uint16x8_t vmlsl_high_u8(uint16x8_t __a, uint8x16_t __b, uint8x16_t __c) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (uint16_t)(__a[__i] - (uint16_t)__b[__i + 8] * (uint16_t)__c[__i + 8]);
  return __r;
}
static __inline__ uint16x4_t vpaddl_u8(uint8x8_t __a) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (uint16_t)((uint16_t)__a[2 * __i] + __a[2 * __i + 1]);
  return __r;
}
static __inline__ uint16x4_t vpadal_u8(uint16x4_t __s, uint8x8_t __a) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (uint16_t)(__s[__i] + (uint16_t)__a[2 * __i] + __a[2 * __i + 1]);
  return __r;
}
static __inline__ uint16_t vaddlv_u8(uint8x8_t __a) {
  uint16_t __r = 0;
  for (int __i = 0; __i < 8; __i++) __r += __a[__i];
  return __r;
}
static __inline__ uint16x8_t vpaddlq_u8(uint8x16_t __a) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (uint16_t)((uint16_t)__a[2 * __i] + __a[2 * __i + 1]);
  return __r;
}
static __inline__ uint16x8_t vpadalq_u8(uint16x8_t __s, uint8x16_t __a) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++)
    __r[__i] = (uint16_t)(__s[__i] + (uint16_t)__a[2 * __i] + __a[2 * __i + 1]);
  return __r;
}
static __inline__ uint16_t vaddlvq_u8(uint8x16_t __a) {
  uint16_t __r = 0;
  for (int __i = 0; __i < 16; __i++) __r += __a[__i];
  return __r;
}
static __inline__ uint16x8_t vshll_n_u8(uint8x8_t __a, const int __k) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint16_t)((uint16_t)__a[__i] << __k);
  return __r;
}
static __inline__ uint16x8_t vshll_high_n_u8(uint8x16_t __a, const int __k) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint16_t)((uint16_t)__a[__i + 8] << __k);
  return __r;
}
static __inline__ uint32x4_t vmovl_u16(uint16x4_t __a) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint32_t)__a[__i];
  return __r;
}
static __inline__ uint32x4_t vmovl_high_u16(uint16x8_t __a) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint32_t)__a[__i + 4];
  return __r;
}
static __inline__ uint16x4_t vmovn_u32(uint32x4_t __a) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint16_t)__a[__i];
  return __r;
}
static __inline__ uint16x8_t vmovn_high_u32(uint16x4_t __lo, uint32x4_t __a) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i < 4 ? __lo[__i] : (uint16_t)__a[__i - 4];
  return __r;
}
static __inline__ uint16x4_t vshrn_n_u32(uint32x4_t __a, const int __k) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint16_t)(__a[__i] >> __k);
  return __r;
}
static __inline__ uint16x4_t vrshrn_n_u32(uint32x4_t __a, const int __k) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (uint16_t)((__a[__i] + ((uint32_t)1 << (__k - 1))) >> __k);
  return __r;
}
static __inline__ uint32x4_t vaddl_u16(uint16x4_t __a, uint16x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint32_t)((uint32_t)__a[__i] + (uint32_t)__b[__i]);
  return __r;
}
static __inline__ uint32x4_t vaddl_high_u16(uint16x8_t __a, uint16x8_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (uint32_t)((uint32_t)__a[__i + 4] + (uint32_t)__b[__i + 4]);
  return __r;
}
static __inline__ uint32x4_t vsubl_u16(uint16x4_t __a, uint16x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint32_t)((uint32_t)__a[__i] - (uint32_t)__b[__i]);
  return __r;
}
static __inline__ uint32x4_t vsubl_high_u16(uint16x8_t __a, uint16x8_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (uint32_t)((uint32_t)__a[__i + 4] - (uint32_t)__b[__i + 4]);
  return __r;
}
static __inline__ uint32x4_t vmull_u16(uint16x4_t __a, uint16x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint32_t)((uint32_t)__a[__i] * (uint32_t)__b[__i]);
  return __r;
}
static __inline__ uint32x4_t vmull_high_u16(uint16x8_t __a, uint16x8_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (uint32_t)((uint32_t)__a[__i + 4] * (uint32_t)__b[__i + 4]);
  return __r;
}
static __inline__ uint32x4_t vmull_n_u16(uint16x4_t __a, uint16_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint32_t)((uint32_t)__a[__i] * (uint32_t)__b);
  return __r;
}
static __inline__ uint32x4_t vabdl_u16(uint16x4_t __a, uint16x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (uint32_t)(__a[__i] > __b[__i] ? (uint32_t)__a[__i] - __b[__i]
        : (uint32_t)__b[__i] - __a[__i]);
  return __r;
}
static __inline__ uint32x4_t vaddw_u16(uint32x4_t __a, uint16x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint32_t)(__a[__i] + (uint32_t)__b[__i]);
  return __r;
}
static __inline__ uint32x4_t vaddw_high_u16(uint32x4_t __a, uint16x8_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint32_t)(__a[__i] + (uint32_t)__b[__i + 4]);
  return __r;
}
static __inline__ uint32x4_t vsubw_u16(uint32x4_t __a, uint16x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint32_t)(__a[__i] - (uint32_t)__b[__i]);
  return __r;
}
static __inline__ uint32x4_t vsubw_high_u16(uint32x4_t __a, uint16x8_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint32_t)(__a[__i] - (uint32_t)__b[__i + 4]);
  return __r;
}
static __inline__ uint32x4_t vmlal_u16(uint32x4_t __a, uint16x4_t __b, uint16x4_t __c) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (uint32_t)(__a[__i] + (uint32_t)__b[__i] * (uint32_t)__c[__i]);
  return __r;
}
static __inline__ uint32x4_t vmlal_high_u16(uint32x4_t __a, uint16x8_t __b, uint16x8_t __c) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (uint32_t)(__a[__i] + (uint32_t)__b[__i + 4] * (uint32_t)__c[__i + 4]);
  return __r;
}
static __inline__ uint32x4_t vmlal_n_u16(uint32x4_t __a, uint16x4_t __b, uint16_t __c) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (uint32_t)(__a[__i] + (uint32_t)__b[__i] * (uint32_t)__c);
  return __r;
}
static __inline__ uint32x4_t vmlsl_u16(uint32x4_t __a, uint16x4_t __b, uint16x4_t __c) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (uint32_t)(__a[__i] - (uint32_t)__b[__i] * (uint32_t)__c[__i]);
  return __r;
}
static __inline__ uint32x4_t vmlsl_high_u16(uint32x4_t __a, uint16x8_t __b, uint16x8_t __c) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (uint32_t)(__a[__i] - (uint32_t)__b[__i + 4] * (uint32_t)__c[__i + 4]);
  return __r;
}
static __inline__ uint32x4_t vmlsl_n_u16(uint32x4_t __a, uint16x4_t __b, uint16_t __c) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (uint32_t)(__a[__i] - (uint32_t)__b[__i] * (uint32_t)__c);
  return __r;
}
static __inline__ uint32x2_t vpaddl_u16(uint16x4_t __a) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (uint32_t)((uint32_t)__a[2 * __i] + __a[2 * __i + 1]);
  return __r;
}
static __inline__ uint32x2_t vpadal_u16(uint32x2_t __s, uint16x4_t __a) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (uint32_t)(__s[__i] + (uint32_t)__a[2 * __i] + __a[2 * __i + 1]);
  return __r;
}
static __inline__ uint32_t vaddlv_u16(uint16x4_t __a) {
  uint32_t __r = 0;
  for (int __i = 0; __i < 4; __i++) __r += __a[__i];
  return __r;
}
static __inline__ uint32x4_t vpaddlq_u16(uint16x8_t __a) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (uint32_t)((uint32_t)__a[2 * __i] + __a[2 * __i + 1]);
  return __r;
}
static __inline__ uint32x4_t vpadalq_u16(uint32x4_t __s, uint16x8_t __a) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = (uint32_t)(__s[__i] + (uint32_t)__a[2 * __i] + __a[2 * __i + 1]);
  return __r;
}
static __inline__ uint32_t vaddlvq_u16(uint16x8_t __a) {
  uint32_t __r = 0;
  for (int __i = 0; __i < 8; __i++) __r += __a[__i];
  return __r;
}
static __inline__ uint32x4_t vshll_n_u16(uint16x4_t __a, const int __k) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint32_t)((uint32_t)__a[__i] << __k);
  return __r;
}
static __inline__ uint32x4_t vshll_high_n_u16(uint16x8_t __a, const int __k) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (uint32_t)((uint32_t)__a[__i + 4] << __k);
  return __r;
}
static __inline__ uint64x2_t vmovl_u32(uint32x2_t __a) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint64_t)__a[__i];
  return __r;
}
static __inline__ uint64x2_t vmovl_high_u32(uint32x4_t __a) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint64_t)__a[__i + 2];
  return __r;
}
static __inline__ uint32x2_t vmovn_u64(uint64x2_t __a) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint32_t)__a[__i];
  return __r;
}
static __inline__ uint32x4_t vmovn_high_u64(uint32x2_t __lo, uint64x2_t __a) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i < 2 ? __lo[__i] : (uint32_t)__a[__i - 2];
  return __r;
}
static __inline__ uint32x2_t vshrn_n_u64(uint64x2_t __a, const int __k) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint32_t)(__a[__i] >> __k);
  return __r;
}
static __inline__ uint32x2_t vrshrn_n_u64(uint64x2_t __a, const int __k) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (uint32_t)((__a[__i] + ((uint64_t)1 << (__k - 1))) >> __k);
  return __r;
}
static __inline__ uint64x2_t vaddl_u32(uint32x2_t __a, uint32x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint64_t)((uint64_t)__a[__i] + (uint64_t)__b[__i]);
  return __r;
}
static __inline__ uint64x2_t vaddl_high_u32(uint32x4_t __a, uint32x4_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (uint64_t)((uint64_t)__a[__i + 2] + (uint64_t)__b[__i + 2]);
  return __r;
}
static __inline__ uint64x2_t vsubl_u32(uint32x2_t __a, uint32x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint64_t)((uint64_t)__a[__i] - (uint64_t)__b[__i]);
  return __r;
}
static __inline__ uint64x2_t vsubl_high_u32(uint32x4_t __a, uint32x4_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (uint64_t)((uint64_t)__a[__i + 2] - (uint64_t)__b[__i + 2]);
  return __r;
}
static __inline__ uint64x2_t vmull_u32(uint32x2_t __a, uint32x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint64_t)((uint64_t)__a[__i] * (uint64_t)__b[__i]);
  return __r;
}
static __inline__ uint64x2_t vmull_high_u32(uint32x4_t __a, uint32x4_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (uint64_t)((uint64_t)__a[__i + 2] * (uint64_t)__b[__i + 2]);
  return __r;
}
static __inline__ uint64x2_t vmull_n_u32(uint32x2_t __a, uint32_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint64_t)((uint64_t)__a[__i] * (uint64_t)__b);
  return __r;
}
static __inline__ uint64x2_t vabdl_u32(uint32x2_t __a, uint32x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (uint64_t)(__a[__i] > __b[__i] ? (uint64_t)__a[__i] - __b[__i]
        : (uint64_t)__b[__i] - __a[__i]);
  return __r;
}
static __inline__ uint64x2_t vaddw_u32(uint64x2_t __a, uint32x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint64_t)(__a[__i] + (uint64_t)__b[__i]);
  return __r;
}
static __inline__ uint64x2_t vaddw_high_u32(uint64x2_t __a, uint32x4_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint64_t)(__a[__i] + (uint64_t)__b[__i + 2]);
  return __r;
}
static __inline__ uint64x2_t vsubw_u32(uint64x2_t __a, uint32x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint64_t)(__a[__i] - (uint64_t)__b[__i]);
  return __r;
}
static __inline__ uint64x2_t vsubw_high_u32(uint64x2_t __a, uint32x4_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint64_t)(__a[__i] - (uint64_t)__b[__i + 2]);
  return __r;
}
static __inline__ uint64x2_t vmlal_u32(uint64x2_t __a, uint32x2_t __b, uint32x2_t __c) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (uint64_t)(__a[__i] + (uint64_t)__b[__i] * (uint64_t)__c[__i]);
  return __r;
}
static __inline__ uint64x2_t vmlal_high_u32(uint64x2_t __a, uint32x4_t __b, uint32x4_t __c) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (uint64_t)(__a[__i] + (uint64_t)__b[__i + 2] * (uint64_t)__c[__i + 2]);
  return __r;
}
static __inline__ uint64x2_t vmlal_n_u32(uint64x2_t __a, uint32x2_t __b, uint32_t __c) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (uint64_t)(__a[__i] + (uint64_t)__b[__i] * (uint64_t)__c);
  return __r;
}
static __inline__ uint64x2_t vmlsl_u32(uint64x2_t __a, uint32x2_t __b, uint32x2_t __c) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (uint64_t)(__a[__i] - (uint64_t)__b[__i] * (uint64_t)__c[__i]);
  return __r;
}
static __inline__ uint64x2_t vmlsl_high_u32(uint64x2_t __a, uint32x4_t __b, uint32x4_t __c) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (uint64_t)(__a[__i] - (uint64_t)__b[__i + 2] * (uint64_t)__c[__i + 2]);
  return __r;
}
static __inline__ uint64x2_t vmlsl_n_u32(uint64x2_t __a, uint32x2_t __b, uint32_t __c) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (uint64_t)(__a[__i] - (uint64_t)__b[__i] * (uint64_t)__c);
  return __r;
}
static __inline__ uint64x1_t vpaddl_u32(uint32x2_t __a) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++)
    __r[__i] = (uint64_t)((uint64_t)__a[2 * __i] + __a[2 * __i + 1]);
  return __r;
}
static __inline__ uint64x1_t vpadal_u32(uint64x1_t __s, uint32x2_t __a) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++)
    __r[__i] = (uint64_t)(__s[__i] + (uint64_t)__a[2 * __i] + __a[2 * __i + 1]);
  return __r;
}
static __inline__ uint64_t vaddlv_u32(uint32x2_t __a) {
  uint64_t __r = 0;
  for (int __i = 0; __i < 2; __i++) __r += __a[__i];
  return __r;
}
static __inline__ uint64x2_t vpaddlq_u32(uint32x4_t __a) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (uint64_t)((uint64_t)__a[2 * __i] + __a[2 * __i + 1]);
  return __r;
}
static __inline__ uint64x2_t vpadalq_u32(uint64x2_t __s, uint32x4_t __a) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = (uint64_t)(__s[__i] + (uint64_t)__a[2 * __i] + __a[2 * __i + 1]);
  return __r;
}
static __inline__ uint64_t vaddlvq_u32(uint32x4_t __a) {
  uint64_t __r = 0;
  for (int __i = 0; __i < 4; __i++) __r += __a[__i];
  return __r;
}
static __inline__ uint64x2_t vshll_n_u32(uint32x2_t __a, const int __k) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint64_t)((uint64_t)__a[__i] << __k);
  return __r;
}
static __inline__ uint64x2_t vshll_high_n_u32(uint32x4_t __a, const int __k) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (uint64_t)((uint64_t)__a[__i + 2] << __k);
  return __r;
}
static __inline__ poly16x8_t vmull_p8(poly8x8_t __a, poly8x8_t __b) {
  poly16x8_t __r = {0};
  for (int __i = 0; __i < 8; __i++)
    for (int __k = 0; __k < 8; __k++)
      if (__b[__i] >> __k & 1) __r[__i] ^= (uint16_t)(__a[__i] << __k);
  return __r;
}

/* Conversions between integers and floats, where a value out of range saturates. */
static __inline__ float32x2_t vcvt_f32_s32(int32x2_t __a) {
  float32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (float32_t)__a[__i];
  return __r;
}
static __inline__ int32x2_t vcvt_s32_f32(float32x2_t __a) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = __a[__i] != __a[__i] ? 0 : __a[__i] >= 0x1p31 ? (int32_t)((uint64_t)-1 >> 33)
        : __a[__i] <= -0x1p31 ? (int32_t)(-(int32_t)((uint64_t)-1 >> 33) - 1) : (int32_t)__a[__i];
  return __r;
}
static __inline__ float32x2_t vcvt_f32_u32(uint32x2_t __a) {
  float32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (float32_t)__a[__i];
  return __r;
}
static __inline__ uint32x2_t vcvt_u32_f32(float32x2_t __a) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = __a[__i] != __a[__i] ? 0 : __a[__i] >= 0x1p32 ? (uint32_t)~(uint32_t)0
        : __a[__i] <= 0 ? 0 : (uint32_t)__a[__i];
  return __r;
}
static __inline__ float64x1_t vcvt_f64_s64(int64x1_t __a) {
  float64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = (float64_t)__a[__i];
  return __r;
}
static __inline__ int64x1_t vcvt_s64_f64(float64x1_t __a) {
  int64x1_t __r;
  for (int __i = 0; __i < 1; __i++)
    __r[__i] = __a[__i] != __a[__i] ? 0 : __a[__i] >= 0x1p63 ? (int64_t)((uint64_t)-1 >> 1)
        : __a[__i] <= -0x1p63 ? (int64_t)(-(int64_t)((uint64_t)-1 >> 1) - 1) : (int64_t)__a[__i];
  return __r;
}
static __inline__ float64x1_t vcvt_f64_u64(uint64x1_t __a) {
  float64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = (float64_t)__a[__i];
  return __r;
}
static __inline__ uint64x1_t vcvt_u64_f64(float64x1_t __a) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++)
    __r[__i] = __a[__i] != __a[__i] ? 0 : __a[__i] >= 0x1p64 ? (uint64_t)~(uint64_t)0
        : __a[__i] <= 0 ? 0 : (uint64_t)__a[__i];
  return __r;
}
static __inline__ float32x4_t vcvtq_f32_s32(int32x4_t __a) {
  float32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (float32_t)__a[__i];
  return __r;
}
static __inline__ int32x4_t vcvtq_s32_f32(float32x4_t __a) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = __a[__i] != __a[__i] ? 0 : __a[__i] >= 0x1p31 ? (int32_t)((uint64_t)-1 >> 33)
        : __a[__i] <= -0x1p31 ? (int32_t)(-(int32_t)((uint64_t)-1 >> 33) - 1) : (int32_t)__a[__i];
  return __r;
}
static __inline__ float32x4_t vcvtq_f32_u32(uint32x4_t __a) {
  float32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = (float32_t)__a[__i];
  return __r;
}
static __inline__ uint32x4_t vcvtq_u32_f32(float32x4_t __a) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++)
    __r[__i] = __a[__i] != __a[__i] ? 0 : __a[__i] >= 0x1p32 ? (uint32_t)~(uint32_t)0
        : __a[__i] <= 0 ? 0 : (uint32_t)__a[__i];
  return __r;
}
static __inline__ float64x2_t vcvtq_f64_s64(int64x2_t __a) {
  float64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (float64_t)__a[__i];
  return __r;
}
static __inline__ int64x2_t vcvtq_s64_f64(float64x2_t __a) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = __a[__i] != __a[__i] ? 0 : __a[__i] >= 0x1p63 ? (int64_t)((uint64_t)-1 >> 1)
        : __a[__i] <= -0x1p63 ? (int64_t)(-(int64_t)((uint64_t)-1 >> 1) - 1) : (int64_t)__a[__i];
  return __r;
}
static __inline__ float64x2_t vcvtq_f64_u64(uint64x2_t __a) {
  float64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (float64_t)__a[__i];
  return __r;
}
static __inline__ uint64x2_t vcvtq_u64_f64(float64x2_t __a) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++)
    __r[__i] = __a[__i] != __a[__i] ? 0 : __a[__i] >= 0x1p64 ? (uint64_t)~(uint64_t)0
        : __a[__i] <= 0 ? 0 : (uint64_t)__a[__i];
  return __r;
}
static __inline__ float64x2_t vcvt_f64_f32(float32x2_t __a) {
  float64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (float64_t)__a[__i];
  return __r;
}
static __inline__ float32x2_t vcvt_f32_f64(float64x2_t __a) {
  float32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = (float32_t)__a[__i];
  return __r;
}

/* Permutes. */
static __inline__ int8x8_t vext_s8(int8x8_t __a, int8x8_t __b, const int __k) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i + __k < 8 ? __a[__i + __k] : __b[__i + __k - 8];
  return __r;
}
static __inline__ int8x8_t vzip1_s8(int8x8_t __a, int8x8_t __b) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i % 2 ? __b[__i / 2] : __a[__i / 2];
  return __r;
}
static __inline__ int8x8_t vzip2_s8(int8x8_t __a, int8x8_t __b) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i % 2 ? __b[4 + __i / 2] : __a[4 + __i / 2];
  return __r;
}
static __inline__ int8x8_t vuzp1_s8(int8x8_t __a, int8x8_t __b) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i < 4 ? __a[2 * __i] : __b[2 * (__i - 4)];
  return __r;
}
static __inline__ int8x8_t vuzp2_s8(int8x8_t __a, int8x8_t __b) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i < 4 ? __a[2 * __i + 1] : __b[2 * (__i - 4) + 1];
  return __r;
}
static __inline__ int8x8_t vtrn1_s8(int8x8_t __a, int8x8_t __b) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i % 2 ? __b[__i - 1] : __a[__i];
  return __r;
}
static __inline__ int8x8_t vtrn2_s8(int8x8_t __a, int8x8_t __b) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i % 2 ? __b[__i] : __a[__i + 1];
  return __r;
}
static __inline__ int8x8x2_t vzip_s8(int8x8_t __a, int8x8_t __b) {
  int8x8x2_t __r;
  __r.val[0] = vzip1_s8(__a, __b);
  __r.val[1] = vzip2_s8(__a, __b);
  return __r;
}
static __inline__ int8x8x2_t vuzp_s8(int8x8_t __a, int8x8_t __b) {
  int8x8x2_t __r;
  __r.val[0] = vuzp1_s8(__a, __b);
  __r.val[1] = vuzp2_s8(__a, __b);
  return __r;
}
static __inline__ int8x8x2_t vtrn_s8(int8x8_t __a, int8x8_t __b) {
  int8x8x2_t __r;
  __r.val[0] = vtrn1_s8(__a, __b);
  __r.val[1] = vtrn2_s8(__a, __b);
  return __r;
}
static __inline__ int8x8_t vrev16_s8(int8x8_t __a) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i - __i % 2 + 2 - 1 - __i % 2];
  return __r;
}
static __inline__ int8x8_t vrev32_s8(int8x8_t __a) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i - __i % 4 + 4 - 1 - __i % 4];
  return __r;
}
static __inline__ int8x8_t vrev64_s8(int8x8_t __a) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i - __i % 8 + 8 - 1 - __i % 8];
  return __r;
}
static __inline__ int16x4_t vext_s16(int16x4_t __a, int16x4_t __b, const int __k) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i + __k < 4 ? __a[__i + __k] : __b[__i + __k - 4];
  return __r;
}
static __inline__ int16x4_t vzip1_s16(int16x4_t __a, int16x4_t __b) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i % 2 ? __b[__i / 2] : __a[__i / 2];
  return __r;
}
static __inline__ int16x4_t vzip2_s16(int16x4_t __a, int16x4_t __b) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i % 2 ? __b[2 + __i / 2] : __a[2 + __i / 2];
  return __r;
}
static __inline__ int16x4_t vuzp1_s16(int16x4_t __a, int16x4_t __b) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i < 2 ? __a[2 * __i] : __b[2 * (__i - 2)];
  return __r;
}
static __inline__ int16x4_t vuzp2_s16(int16x4_t __a, int16x4_t __b) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i < 2 ? __a[2 * __i + 1] : __b[2 * (__i - 2) + 1];
  return __r;
}
static __inline__ int16x4_t vtrn1_s16(int16x4_t __a, int16x4_t __b) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i % 2 ? __b[__i - 1] : __a[__i];
  return __r;
}
static __inline__ int16x4_t vtrn2_s16(int16x4_t __a, int16x4_t __b) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i % 2 ? __b[__i] : __a[__i + 1];
  return __r;
}
static __inline__ int16x4x2_t vzip_s16(int16x4_t __a, int16x4_t __b) {
  int16x4x2_t __r;
  __r.val[0] = vzip1_s16(__a, __b);
  __r.val[1] = vzip2_s16(__a, __b);
  return __r;
}
static __inline__ int16x4x2_t vuzp_s16(int16x4_t __a, int16x4_t __b) {
  int16x4x2_t __r;
  __r.val[0] = vuzp1_s16(__a, __b);
  __r.val[1] = vuzp2_s16(__a, __b);
  return __r;
}
static __inline__ int16x4x2_t vtrn_s16(int16x4_t __a, int16x4_t __b) {
  int16x4x2_t __r;
  __r.val[0] = vtrn1_s16(__a, __b);
  __r.val[1] = vtrn2_s16(__a, __b);
  return __r;
}
static __inline__ int16x4_t vrev32_s16(int16x4_t __a) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i - __i % 2 + 2 - 1 - __i % 2];
  return __r;
}
static __inline__ int16x4_t vrev64_s16(int16x4_t __a) {
  int16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i - __i % 4 + 4 - 1 - __i % 4];
  return __r;
}
static __inline__ int32x2_t vext_s32(int32x2_t __a, int32x2_t __b, const int __k) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i + __k < 2 ? __a[__i + __k] : __b[__i + __k - 2];
  return __r;
}
static __inline__ int32x2_t vzip1_s32(int32x2_t __a, int32x2_t __b) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i % 2 ? __b[__i / 2] : __a[__i / 2];
  return __r;
}
static __inline__ int32x2_t vzip2_s32(int32x2_t __a, int32x2_t __b) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i % 2 ? __b[1 + __i / 2] : __a[1 + __i / 2];
  return __r;
}
static __inline__ int32x2_t vuzp1_s32(int32x2_t __a, int32x2_t __b) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i < 1 ? __a[2 * __i] : __b[2 * (__i - 1)];
  return __r;
}
static __inline__ int32x2_t vuzp2_s32(int32x2_t __a, int32x2_t __b) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i < 1 ? __a[2 * __i + 1] : __b[2 * (__i - 1) + 1];
  return __r;
}
static __inline__ int32x2_t vtrn1_s32(int32x2_t __a, int32x2_t __b) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i % 2 ? __b[__i - 1] : __a[__i];
  return __r;
}
static __inline__ int32x2_t vtrn2_s32(int32x2_t __a, int32x2_t __b) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i % 2 ? __b[__i] : __a[__i + 1];
  return __r;
}
static __inline__ int32x2x2_t vzip_s32(int32x2_t __a, int32x2_t __b) {
  int32x2x2_t __r;
  __r.val[0] = vzip1_s32(__a, __b);
  __r.val[1] = vzip2_s32(__a, __b);
  return __r;
}
static __inline__ int32x2x2_t vuzp_s32(int32x2_t __a, int32x2_t __b) {
  int32x2x2_t __r;
  __r.val[0] = vuzp1_s32(__a, __b);
  __r.val[1] = vuzp2_s32(__a, __b);
  return __r;
}
static __inline__ int32x2x2_t vtrn_s32(int32x2_t __a, int32x2_t __b) {
  int32x2x2_t __r;
  __r.val[0] = vtrn1_s32(__a, __b);
  __r.val[1] = vtrn2_s32(__a, __b);
  return __r;
}
static __inline__ int32x2_t vrev64_s32(int32x2_t __a) {
  int32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i - __i % 2 + 2 - 1 - __i % 2];
  return __r;
}
static __inline__ int64x1_t vext_s64(int64x1_t __a, int64x1_t __b, const int __k) {
  int64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __i + __k < 1 ? __a[__i + __k] : __b[__i + __k - 1];
  return __r;
}
static __inline__ uint8x8_t vext_u8(uint8x8_t __a, uint8x8_t __b, const int __k) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i + __k < 8 ? __a[__i + __k] : __b[__i + __k - 8];
  return __r;
}
static __inline__ uint8x8_t vzip1_u8(uint8x8_t __a, uint8x8_t __b) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i % 2 ? __b[__i / 2] : __a[__i / 2];
  return __r;
}
static __inline__ uint8x8_t vzip2_u8(uint8x8_t __a, uint8x8_t __b) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i % 2 ? __b[4 + __i / 2] : __a[4 + __i / 2];
  return __r;
}
static __inline__ uint8x8_t vuzp1_u8(uint8x8_t __a, uint8x8_t __b) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i < 4 ? __a[2 * __i] : __b[2 * (__i - 4)];
  return __r;
}
static __inline__ uint8x8_t vuzp2_u8(uint8x8_t __a, uint8x8_t __b) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i < 4 ? __a[2 * __i + 1] : __b[2 * (__i - 4) + 1];
  return __r;
}
static __inline__ uint8x8_t vtrn1_u8(uint8x8_t __a, uint8x8_t __b) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i % 2 ? __b[__i - 1] : __a[__i];
  return __r;
}
static __inline__ uint8x8_t vtrn2_u8(uint8x8_t __a, uint8x8_t __b) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i % 2 ? __b[__i] : __a[__i + 1];
  return __r;
}
static __inline__ uint8x8x2_t vzip_u8(uint8x8_t __a, uint8x8_t __b) {
  uint8x8x2_t __r;
  __r.val[0] = vzip1_u8(__a, __b);
  __r.val[1] = vzip2_u8(__a, __b);
  return __r;
}
static __inline__ uint8x8x2_t vuzp_u8(uint8x8_t __a, uint8x8_t __b) {
  uint8x8x2_t __r;
  __r.val[0] = vuzp1_u8(__a, __b);
  __r.val[1] = vuzp2_u8(__a, __b);
  return __r;
}
static __inline__ uint8x8x2_t vtrn_u8(uint8x8_t __a, uint8x8_t __b) {
  uint8x8x2_t __r;
  __r.val[0] = vtrn1_u8(__a, __b);
  __r.val[1] = vtrn2_u8(__a, __b);
  return __r;
}
static __inline__ uint8x8_t vrev16_u8(uint8x8_t __a) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i - __i % 2 + 2 - 1 - __i % 2];
  return __r;
}
static __inline__ uint8x8_t vrev32_u8(uint8x8_t __a) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i - __i % 4 + 4 - 1 - __i % 4];
  return __r;
}
static __inline__ uint8x8_t vrev64_u8(uint8x8_t __a) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i - __i % 8 + 8 - 1 - __i % 8];
  return __r;
}
static __inline__ uint16x4_t vext_u16(uint16x4_t __a, uint16x4_t __b, const int __k) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i + __k < 4 ? __a[__i + __k] : __b[__i + __k - 4];
  return __r;
}
static __inline__ uint16x4_t vzip1_u16(uint16x4_t __a, uint16x4_t __b) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i % 2 ? __b[__i / 2] : __a[__i / 2];
  return __r;
}
static __inline__ uint16x4_t vzip2_u16(uint16x4_t __a, uint16x4_t __b) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i % 2 ? __b[2 + __i / 2] : __a[2 + __i / 2];
  return __r;
}
static __inline__ uint16x4_t vuzp1_u16(uint16x4_t __a, uint16x4_t __b) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i < 2 ? __a[2 * __i] : __b[2 * (__i - 2)];
  return __r;
}
static __inline__ uint16x4_t vuzp2_u16(uint16x4_t __a, uint16x4_t __b) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i < 2 ? __a[2 * __i + 1] : __b[2 * (__i - 2) + 1];
  return __r;
}
static __inline__ uint16x4_t vtrn1_u16(uint16x4_t __a, uint16x4_t __b) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i % 2 ? __b[__i - 1] : __a[__i];
  return __r;
}
static __inline__ uint16x4_t vtrn2_u16(uint16x4_t __a, uint16x4_t __b) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i % 2 ? __b[__i] : __a[__i + 1];
  return __r;
}
static __inline__ uint16x4x2_t vzip_u16(uint16x4_t __a, uint16x4_t __b) {
  uint16x4x2_t __r;
  __r.val[0] = vzip1_u16(__a, __b);
  __r.val[1] = vzip2_u16(__a, __b);
  return __r;
}
static __inline__ uint16x4x2_t vuzp_u16(uint16x4_t __a, uint16x4_t __b) {
  uint16x4x2_t __r;
  __r.val[0] = vuzp1_u16(__a, __b);
  __r.val[1] = vuzp2_u16(__a, __b);
  return __r;
}
static __inline__ uint16x4x2_t vtrn_u16(uint16x4_t __a, uint16x4_t __b) {
  uint16x4x2_t __r;
  __r.val[0] = vtrn1_u16(__a, __b);
  __r.val[1] = vtrn2_u16(__a, __b);
  return __r;
}
static __inline__ uint16x4_t vrev32_u16(uint16x4_t __a) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i - __i % 2 + 2 - 1 - __i % 2];
  return __r;
}
static __inline__ uint16x4_t vrev64_u16(uint16x4_t __a) {
  uint16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i - __i % 4 + 4 - 1 - __i % 4];
  return __r;
}
static __inline__ uint32x2_t vext_u32(uint32x2_t __a, uint32x2_t __b, const int __k) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i + __k < 2 ? __a[__i + __k] : __b[__i + __k - 2];
  return __r;
}
static __inline__ uint32x2_t vzip1_u32(uint32x2_t __a, uint32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i % 2 ? __b[__i / 2] : __a[__i / 2];
  return __r;
}
static __inline__ uint32x2_t vzip2_u32(uint32x2_t __a, uint32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i % 2 ? __b[1 + __i / 2] : __a[1 + __i / 2];
  return __r;
}
static __inline__ uint32x2_t vuzp1_u32(uint32x2_t __a, uint32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i < 1 ? __a[2 * __i] : __b[2 * (__i - 1)];
  return __r;
}
static __inline__ uint32x2_t vuzp2_u32(uint32x2_t __a, uint32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i < 1 ? __a[2 * __i + 1] : __b[2 * (__i - 1) + 1];
  return __r;
}
static __inline__ uint32x2_t vtrn1_u32(uint32x2_t __a, uint32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i % 2 ? __b[__i - 1] : __a[__i];
  return __r;
}
static __inline__ uint32x2_t vtrn2_u32(uint32x2_t __a, uint32x2_t __b) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i % 2 ? __b[__i] : __a[__i + 1];
  return __r;
}
static __inline__ uint32x2x2_t vzip_u32(uint32x2_t __a, uint32x2_t __b) {
  uint32x2x2_t __r;
  __r.val[0] = vzip1_u32(__a, __b);
  __r.val[1] = vzip2_u32(__a, __b);
  return __r;
}
static __inline__ uint32x2x2_t vuzp_u32(uint32x2_t __a, uint32x2_t __b) {
  uint32x2x2_t __r;
  __r.val[0] = vuzp1_u32(__a, __b);
  __r.val[1] = vuzp2_u32(__a, __b);
  return __r;
}
static __inline__ uint32x2x2_t vtrn_u32(uint32x2_t __a, uint32x2_t __b) {
  uint32x2x2_t __r;
  __r.val[0] = vtrn1_u32(__a, __b);
  __r.val[1] = vtrn2_u32(__a, __b);
  return __r;
}
static __inline__ uint32x2_t vrev64_u32(uint32x2_t __a) {
  uint32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i - __i % 2 + 2 - 1 - __i % 2];
  return __r;
}
static __inline__ uint64x1_t vext_u64(uint64x1_t __a, uint64x1_t __b, const int __k) {
  uint64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __i + __k < 1 ? __a[__i + __k] : __b[__i + __k - 1];
  return __r;
}
static __inline__ float32x2_t vext_f32(float32x2_t __a, float32x2_t __b, const int __k) {
  float32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i + __k < 2 ? __a[__i + __k] : __b[__i + __k - 2];
  return __r;
}
static __inline__ float32x2_t vzip1_f32(float32x2_t __a, float32x2_t __b) {
  float32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i % 2 ? __b[__i / 2] : __a[__i / 2];
  return __r;
}
static __inline__ float32x2_t vzip2_f32(float32x2_t __a, float32x2_t __b) {
  float32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i % 2 ? __b[1 + __i / 2] : __a[1 + __i / 2];
  return __r;
}
static __inline__ float32x2_t vuzp1_f32(float32x2_t __a, float32x2_t __b) {
  float32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i < 1 ? __a[2 * __i] : __b[2 * (__i - 1)];
  return __r;
}
static __inline__ float32x2_t vuzp2_f32(float32x2_t __a, float32x2_t __b) {
  float32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i < 1 ? __a[2 * __i + 1] : __b[2 * (__i - 1) + 1];
  return __r;
}
static __inline__ float32x2_t vtrn1_f32(float32x2_t __a, float32x2_t __b) {
  float32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i % 2 ? __b[__i - 1] : __a[__i];
  return __r;
}
static __inline__ float32x2_t vtrn2_f32(float32x2_t __a, float32x2_t __b) {
  float32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i % 2 ? __b[__i] : __a[__i + 1];
  return __r;
}
static __inline__ float32x2x2_t vzip_f32(float32x2_t __a, float32x2_t __b) {
  float32x2x2_t __r;
  __r.val[0] = vzip1_f32(__a, __b);
  __r.val[1] = vzip2_f32(__a, __b);
  return __r;
}
static __inline__ float32x2x2_t vuzp_f32(float32x2_t __a, float32x2_t __b) {
  float32x2x2_t __r;
  __r.val[0] = vuzp1_f32(__a, __b);
  __r.val[1] = vuzp2_f32(__a, __b);
  return __r;
}
static __inline__ float32x2x2_t vtrn_f32(float32x2_t __a, float32x2_t __b) {
  float32x2x2_t __r;
  __r.val[0] = vtrn1_f32(__a, __b);
  __r.val[1] = vtrn2_f32(__a, __b);
  return __r;
}
static __inline__ float32x2_t vrev64_f32(float32x2_t __a) {
  float32x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __a[__i - __i % 2 + 2 - 1 - __i % 2];
  return __r;
}
static __inline__ float64x1_t vext_f64(float64x1_t __a, float64x1_t __b, const int __k) {
  float64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __i + __k < 1 ? __a[__i + __k] : __b[__i + __k - 1];
  return __r;
}
static __inline__ poly8x8_t vext_p8(poly8x8_t __a, poly8x8_t __b, const int __k) {
  poly8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i + __k < 8 ? __a[__i + __k] : __b[__i + __k - 8];
  return __r;
}
static __inline__ poly8x8_t vzip1_p8(poly8x8_t __a, poly8x8_t __b) {
  poly8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i % 2 ? __b[__i / 2] : __a[__i / 2];
  return __r;
}
static __inline__ poly8x8_t vzip2_p8(poly8x8_t __a, poly8x8_t __b) {
  poly8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i % 2 ? __b[4 + __i / 2] : __a[4 + __i / 2];
  return __r;
}
static __inline__ poly8x8_t vuzp1_p8(poly8x8_t __a, poly8x8_t __b) {
  poly8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i < 4 ? __a[2 * __i] : __b[2 * (__i - 4)];
  return __r;
}
static __inline__ poly8x8_t vuzp2_p8(poly8x8_t __a, poly8x8_t __b) {
  poly8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i < 4 ? __a[2 * __i + 1] : __b[2 * (__i - 4) + 1];
  return __r;
}
static __inline__ poly8x8_t vtrn1_p8(poly8x8_t __a, poly8x8_t __b) {
  poly8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i % 2 ? __b[__i - 1] : __a[__i];
  return __r;
}
static __inline__ poly8x8_t vtrn2_p8(poly8x8_t __a, poly8x8_t __b) {
  poly8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i % 2 ? __b[__i] : __a[__i + 1];
  return __r;
}
static __inline__ poly8x8x2_t vzip_p8(poly8x8_t __a, poly8x8_t __b) {
  poly8x8x2_t __r;
  __r.val[0] = vzip1_p8(__a, __b);
  __r.val[1] = vzip2_p8(__a, __b);
  return __r;
}
static __inline__ poly8x8x2_t vuzp_p8(poly8x8_t __a, poly8x8_t __b) {
  poly8x8x2_t __r;
  __r.val[0] = vuzp1_p8(__a, __b);
  __r.val[1] = vuzp2_p8(__a, __b);
  return __r;
}
static __inline__ poly8x8x2_t vtrn_p8(poly8x8_t __a, poly8x8_t __b) {
  poly8x8x2_t __r;
  __r.val[0] = vtrn1_p8(__a, __b);
  __r.val[1] = vtrn2_p8(__a, __b);
  return __r;
}
static __inline__ poly8x8_t vrev16_p8(poly8x8_t __a) {
  poly8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i - __i % 2 + 2 - 1 - __i % 2];
  return __r;
}
static __inline__ poly8x8_t vrev32_p8(poly8x8_t __a) {
  poly8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i - __i % 4 + 4 - 1 - __i % 4];
  return __r;
}
static __inline__ poly8x8_t vrev64_p8(poly8x8_t __a) {
  poly8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i - __i % 8 + 8 - 1 - __i % 8];
  return __r;
}
static __inline__ poly16x4_t vext_p16(poly16x4_t __a, poly16x4_t __b, const int __k) {
  poly16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i + __k < 4 ? __a[__i + __k] : __b[__i + __k - 4];
  return __r;
}
static __inline__ poly16x4_t vzip1_p16(poly16x4_t __a, poly16x4_t __b) {
  poly16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i % 2 ? __b[__i / 2] : __a[__i / 2];
  return __r;
}
static __inline__ poly16x4_t vzip2_p16(poly16x4_t __a, poly16x4_t __b) {
  poly16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i % 2 ? __b[2 + __i / 2] : __a[2 + __i / 2];
  return __r;
}
static __inline__ poly16x4_t vuzp1_p16(poly16x4_t __a, poly16x4_t __b) {
  poly16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i < 2 ? __a[2 * __i] : __b[2 * (__i - 2)];
  return __r;
}
static __inline__ poly16x4_t vuzp2_p16(poly16x4_t __a, poly16x4_t __b) {
  poly16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i < 2 ? __a[2 * __i + 1] : __b[2 * (__i - 2) + 1];
  return __r;
}
static __inline__ poly16x4_t vtrn1_p16(poly16x4_t __a, poly16x4_t __b) {
  poly16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i % 2 ? __b[__i - 1] : __a[__i];
  return __r;
}
static __inline__ poly16x4_t vtrn2_p16(poly16x4_t __a, poly16x4_t __b) {
  poly16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i % 2 ? __b[__i] : __a[__i + 1];
  return __r;
}
static __inline__ poly16x4x2_t vzip_p16(poly16x4_t __a, poly16x4_t __b) {
  poly16x4x2_t __r;
  __r.val[0] = vzip1_p16(__a, __b);
  __r.val[1] = vzip2_p16(__a, __b);
  return __r;
}
static __inline__ poly16x4x2_t vuzp_p16(poly16x4_t __a, poly16x4_t __b) {
  poly16x4x2_t __r;
  __r.val[0] = vuzp1_p16(__a, __b);
  __r.val[1] = vuzp2_p16(__a, __b);
  return __r;
}
static __inline__ poly16x4x2_t vtrn_p16(poly16x4_t __a, poly16x4_t __b) {
  poly16x4x2_t __r;
  __r.val[0] = vtrn1_p16(__a, __b);
  __r.val[1] = vtrn2_p16(__a, __b);
  return __r;
}
static __inline__ poly16x4_t vrev32_p16(poly16x4_t __a) {
  poly16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i - __i % 2 + 2 - 1 - __i % 2];
  return __r;
}
static __inline__ poly16x4_t vrev64_p16(poly16x4_t __a) {
  poly16x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i - __i % 4 + 4 - 1 - __i % 4];
  return __r;
}
static __inline__ poly64x1_t vext_p64(poly64x1_t __a, poly64x1_t __b, const int __k) {
  poly64x1_t __r;
  for (int __i = 0; __i < 1; __i++) __r[__i] = __i + __k < 1 ? __a[__i + __k] : __b[__i + __k - 1];
  return __r;
}
static __inline__ int8x16_t vextq_s8(int8x16_t __a, int8x16_t __b, const int __k) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++)
    __r[__i] = __i + __k < 16 ? __a[__i + __k] : __b[__i + __k - 16];
  return __r;
}
static __inline__ int8x16_t vzip1q_s8(int8x16_t __a, int8x16_t __b) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __i % 2 ? __b[__i / 2] : __a[__i / 2];
  return __r;
}
static __inline__ int8x16_t vzip2q_s8(int8x16_t __a, int8x16_t __b) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __i % 2 ? __b[8 + __i / 2] : __a[8 + __i / 2];
  return __r;
}
static __inline__ int8x16_t vuzp1q_s8(int8x16_t __a, int8x16_t __b) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __i < 8 ? __a[2 * __i] : __b[2 * (__i - 8)];
  return __r;
}
static __inline__ int8x16_t vuzp2q_s8(int8x16_t __a, int8x16_t __b) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __i < 8 ? __a[2 * __i + 1] : __b[2 * (__i - 8) + 1];
  return __r;
}
static __inline__ int8x16_t vtrn1q_s8(int8x16_t __a, int8x16_t __b) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __i % 2 ? __b[__i - 1] : __a[__i];
  return __r;
}
static __inline__ int8x16_t vtrn2q_s8(int8x16_t __a, int8x16_t __b) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __i % 2 ? __b[__i] : __a[__i + 1];
  return __r;
}
static __inline__ int8x16x2_t vzipq_s8(int8x16_t __a, int8x16_t __b) {
  int8x16x2_t __r;
  __r.val[0] = vzip1q_s8(__a, __b);
  __r.val[1] = vzip2q_s8(__a, __b);
  return __r;
}
static __inline__ int8x16x2_t vuzpq_s8(int8x16_t __a, int8x16_t __b) {
  int8x16x2_t __r;
  __r.val[0] = vuzp1q_s8(__a, __b);
  __r.val[1] = vuzp2q_s8(__a, __b);
  return __r;
}
static __inline__ int8x16x2_t vtrnq_s8(int8x16_t __a, int8x16_t __b) {
  int8x16x2_t __r;
  __r.val[0] = vtrn1q_s8(__a, __b);
  __r.val[1] = vtrn2q_s8(__a, __b);
  return __r;
}
static __inline__ int8x16_t vrev16q_s8(int8x16_t __a) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __a[__i - __i % 2 + 2 - 1 - __i % 2];
  return __r;
}
static __inline__ int8x16_t vrev32q_s8(int8x16_t __a) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __a[__i - __i % 4 + 4 - 1 - __i % 4];
  return __r;
}
static __inline__ int8x16_t vrev64q_s8(int8x16_t __a) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __a[__i - __i % 8 + 8 - 1 - __i % 8];
  return __r;
}
static __inline__ int16x8_t vextq_s16(int16x8_t __a, int16x8_t __b, const int __k) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i + __k < 8 ? __a[__i + __k] : __b[__i + __k - 8];
  return __r;
}
static __inline__ int16x8_t vzip1q_s16(int16x8_t __a, int16x8_t __b) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i % 2 ? __b[__i / 2] : __a[__i / 2];
  return __r;
}
static __inline__ int16x8_t vzip2q_s16(int16x8_t __a, int16x8_t __b) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i % 2 ? __b[4 + __i / 2] : __a[4 + __i / 2];
  return __r;
}
static __inline__ int16x8_t vuzp1q_s16(int16x8_t __a, int16x8_t __b) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i < 4 ? __a[2 * __i] : __b[2 * (__i - 4)];
  return __r;
}
static __inline__ int16x8_t vuzp2q_s16(int16x8_t __a, int16x8_t __b) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i < 4 ? __a[2 * __i + 1] : __b[2 * (__i - 4) + 1];
  return __r;
}
static __inline__ int16x8_t vtrn1q_s16(int16x8_t __a, int16x8_t __b) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i % 2 ? __b[__i - 1] : __a[__i];
  return __r;
}
static __inline__ int16x8_t vtrn2q_s16(int16x8_t __a, int16x8_t __b) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i % 2 ? __b[__i] : __a[__i + 1];
  return __r;
}
static __inline__ int16x8x2_t vzipq_s16(int16x8_t __a, int16x8_t __b) {
  int16x8x2_t __r;
  __r.val[0] = vzip1q_s16(__a, __b);
  __r.val[1] = vzip2q_s16(__a, __b);
  return __r;
}
static __inline__ int16x8x2_t vuzpq_s16(int16x8_t __a, int16x8_t __b) {
  int16x8x2_t __r;
  __r.val[0] = vuzp1q_s16(__a, __b);
  __r.val[1] = vuzp2q_s16(__a, __b);
  return __r;
}
static __inline__ int16x8x2_t vtrnq_s16(int16x8_t __a, int16x8_t __b) {
  int16x8x2_t __r;
  __r.val[0] = vtrn1q_s16(__a, __b);
  __r.val[1] = vtrn2q_s16(__a, __b);
  return __r;
}
static __inline__ int16x8_t vrev32q_s16(int16x8_t __a) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i - __i % 2 + 2 - 1 - __i % 2];
  return __r;
}
static __inline__ int16x8_t vrev64q_s16(int16x8_t __a) {
  int16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i - __i % 4 + 4 - 1 - __i % 4];
  return __r;
}
static __inline__ int32x4_t vextq_s32(int32x4_t __a, int32x4_t __b, const int __k) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i + __k < 4 ? __a[__i + __k] : __b[__i + __k - 4];
  return __r;
}
static __inline__ int32x4_t vzip1q_s32(int32x4_t __a, int32x4_t __b) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i % 2 ? __b[__i / 2] : __a[__i / 2];
  return __r;
}
static __inline__ int32x4_t vzip2q_s32(int32x4_t __a, int32x4_t __b) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i % 2 ? __b[2 + __i / 2] : __a[2 + __i / 2];
  return __r;
}
static __inline__ int32x4_t vuzp1q_s32(int32x4_t __a, int32x4_t __b) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i < 2 ? __a[2 * __i] : __b[2 * (__i - 2)];
  return __r;
}
static __inline__ int32x4_t vuzp2q_s32(int32x4_t __a, int32x4_t __b) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i < 2 ? __a[2 * __i + 1] : __b[2 * (__i - 2) + 1];
  return __r;
}
static __inline__ int32x4_t vtrn1q_s32(int32x4_t __a, int32x4_t __b) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i % 2 ? __b[__i - 1] : __a[__i];
  return __r;
}
static __inline__ int32x4_t vtrn2q_s32(int32x4_t __a, int32x4_t __b) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i % 2 ? __b[__i] : __a[__i + 1];
  return __r;
}
static __inline__ int32x4x2_t vzipq_s32(int32x4_t __a, int32x4_t __b) {
  int32x4x2_t __r;
  __r.val[0] = vzip1q_s32(__a, __b);
  __r.val[1] = vzip2q_s32(__a, __b);
  return __r;
}
static __inline__ int32x4x2_t vuzpq_s32(int32x4_t __a, int32x4_t __b) {
  int32x4x2_t __r;
  __r.val[0] = vuzp1q_s32(__a, __b);
  __r.val[1] = vuzp2q_s32(__a, __b);
  return __r;
}
static __inline__ int32x4x2_t vtrnq_s32(int32x4_t __a, int32x4_t __b) {
  int32x4x2_t __r;
  __r.val[0] = vtrn1q_s32(__a, __b);
  __r.val[1] = vtrn2q_s32(__a, __b);
  return __r;
}
static __inline__ int32x4_t vrev64q_s32(int32x4_t __a) {
  int32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i - __i % 2 + 2 - 1 - __i % 2];
  return __r;
}
static __inline__ int64x2_t vextq_s64(int64x2_t __a, int64x2_t __b, const int __k) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i + __k < 2 ? __a[__i + __k] : __b[__i + __k - 2];
  return __r;
}
static __inline__ int64x2_t vzip1q_s64(int64x2_t __a, int64x2_t __b) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i % 2 ? __b[__i / 2] : __a[__i / 2];
  return __r;
}
static __inline__ int64x2_t vzip2q_s64(int64x2_t __a, int64x2_t __b) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i % 2 ? __b[1 + __i / 2] : __a[1 + __i / 2];
  return __r;
}
static __inline__ int64x2_t vuzp1q_s64(int64x2_t __a, int64x2_t __b) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i < 1 ? __a[2 * __i] : __b[2 * (__i - 1)];
  return __r;
}
static __inline__ int64x2_t vuzp2q_s64(int64x2_t __a, int64x2_t __b) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i < 1 ? __a[2 * __i + 1] : __b[2 * (__i - 1) + 1];
  return __r;
}
static __inline__ int64x2_t vtrn1q_s64(int64x2_t __a, int64x2_t __b) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i % 2 ? __b[__i - 1] : __a[__i];
  return __r;
}
static __inline__ int64x2_t vtrn2q_s64(int64x2_t __a, int64x2_t __b) {
  int64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i % 2 ? __b[__i] : __a[__i + 1];
  return __r;
}
static __inline__ uint8x16_t vextq_u8(uint8x16_t __a, uint8x16_t __b, const int __k) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++)
    __r[__i] = __i + __k < 16 ? __a[__i + __k] : __b[__i + __k - 16];
  return __r;
}
static __inline__ uint8x16_t vzip1q_u8(uint8x16_t __a, uint8x16_t __b) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __i % 2 ? __b[__i / 2] : __a[__i / 2];
  return __r;
}
static __inline__ uint8x16_t vzip2q_u8(uint8x16_t __a, uint8x16_t __b) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __i % 2 ? __b[8 + __i / 2] : __a[8 + __i / 2];
  return __r;
}
static __inline__ uint8x16_t vuzp1q_u8(uint8x16_t __a, uint8x16_t __b) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __i < 8 ? __a[2 * __i] : __b[2 * (__i - 8)];
  return __r;
}
static __inline__ uint8x16_t vuzp2q_u8(uint8x16_t __a, uint8x16_t __b) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __i < 8 ? __a[2 * __i + 1] : __b[2 * (__i - 8) + 1];
  return __r;
}
static __inline__ uint8x16_t vtrn1q_u8(uint8x16_t __a, uint8x16_t __b) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __i % 2 ? __b[__i - 1] : __a[__i];
  return __r;
}
static __inline__ uint8x16_t vtrn2q_u8(uint8x16_t __a, uint8x16_t __b) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __i % 2 ? __b[__i] : __a[__i + 1];
  return __r;
}
static __inline__ uint8x16x2_t vzipq_u8(uint8x16_t __a, uint8x16_t __b) {
  uint8x16x2_t __r;
  __r.val[0] = vzip1q_u8(__a, __b);
  __r.val[1] = vzip2q_u8(__a, __b);
  return __r;
}
static __inline__ uint8x16x2_t vuzpq_u8(uint8x16_t __a, uint8x16_t __b) {
  uint8x16x2_t __r;
  __r.val[0] = vuzp1q_u8(__a, __b);
  __r.val[1] = vuzp2q_u8(__a, __b);
  return __r;
}
static __inline__ uint8x16x2_t vtrnq_u8(uint8x16_t __a, uint8x16_t __b) {
  uint8x16x2_t __r;
  __r.val[0] = vtrn1q_u8(__a, __b);
  __r.val[1] = vtrn2q_u8(__a, __b);
  return __r;
}
static __inline__ uint8x16_t vrev16q_u8(uint8x16_t __a) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __a[__i - __i % 2 + 2 - 1 - __i % 2];
  return __r;
}
static __inline__ uint8x16_t vrev32q_u8(uint8x16_t __a) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __a[__i - __i % 4 + 4 - 1 - __i % 4];
  return __r;
}
static __inline__ uint8x16_t vrev64q_u8(uint8x16_t __a) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __a[__i - __i % 8 + 8 - 1 - __i % 8];
  return __r;
}
static __inline__ uint16x8_t vextq_u16(uint16x8_t __a, uint16x8_t __b, const int __k) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i + __k < 8 ? __a[__i + __k] : __b[__i + __k - 8];
  return __r;
}
static __inline__ uint16x8_t vzip1q_u16(uint16x8_t __a, uint16x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i % 2 ? __b[__i / 2] : __a[__i / 2];
  return __r;
}
static __inline__ uint16x8_t vzip2q_u16(uint16x8_t __a, uint16x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i % 2 ? __b[4 + __i / 2] : __a[4 + __i / 2];
  return __r;
}
static __inline__ uint16x8_t vuzp1q_u16(uint16x8_t __a, uint16x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i < 4 ? __a[2 * __i] : __b[2 * (__i - 4)];
  return __r;
}
static __inline__ uint16x8_t vuzp2q_u16(uint16x8_t __a, uint16x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i < 4 ? __a[2 * __i + 1] : __b[2 * (__i - 4) + 1];
  return __r;
}
static __inline__ uint16x8_t vtrn1q_u16(uint16x8_t __a, uint16x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i % 2 ? __b[__i - 1] : __a[__i];
  return __r;
}
static __inline__ uint16x8_t vtrn2q_u16(uint16x8_t __a, uint16x8_t __b) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i % 2 ? __b[__i] : __a[__i + 1];
  return __r;
}
static __inline__ uint16x8x2_t vzipq_u16(uint16x8_t __a, uint16x8_t __b) {
  uint16x8x2_t __r;
  __r.val[0] = vzip1q_u16(__a, __b);
  __r.val[1] = vzip2q_u16(__a, __b);
  return __r;
}
static __inline__ uint16x8x2_t vuzpq_u16(uint16x8_t __a, uint16x8_t __b) {
  uint16x8x2_t __r;
  __r.val[0] = vuzp1q_u16(__a, __b);
  __r.val[1] = vuzp2q_u16(__a, __b);
  return __r;
}
static __inline__ uint16x8x2_t vtrnq_u16(uint16x8_t __a, uint16x8_t __b) {
  uint16x8x2_t __r;
  __r.val[0] = vtrn1q_u16(__a, __b);
  __r.val[1] = vtrn2q_u16(__a, __b);
  return __r;
}
static __inline__ uint16x8_t vrev32q_u16(uint16x8_t __a) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i - __i % 2 + 2 - 1 - __i % 2];
  return __r;
}
static __inline__ uint16x8_t vrev64q_u16(uint16x8_t __a) {
  uint16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i - __i % 4 + 4 - 1 - __i % 4];
  return __r;
}
static __inline__ uint32x4_t vextq_u32(uint32x4_t __a, uint32x4_t __b, const int __k) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i + __k < 4 ? __a[__i + __k] : __b[__i + __k - 4];
  return __r;
}
static __inline__ uint32x4_t vzip1q_u32(uint32x4_t __a, uint32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i % 2 ? __b[__i / 2] : __a[__i / 2];
  return __r;
}
static __inline__ uint32x4_t vzip2q_u32(uint32x4_t __a, uint32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i % 2 ? __b[2 + __i / 2] : __a[2 + __i / 2];
  return __r;
}
static __inline__ uint32x4_t vuzp1q_u32(uint32x4_t __a, uint32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i < 2 ? __a[2 * __i] : __b[2 * (__i - 2)];
  return __r;
}
static __inline__ uint32x4_t vuzp2q_u32(uint32x4_t __a, uint32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i < 2 ? __a[2 * __i + 1] : __b[2 * (__i - 2) + 1];
  return __r;
}
static __inline__ uint32x4_t vtrn1q_u32(uint32x4_t __a, uint32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i % 2 ? __b[__i - 1] : __a[__i];
  return __r;
}
static __inline__ uint32x4_t vtrn2q_u32(uint32x4_t __a, uint32x4_t __b) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i % 2 ? __b[__i] : __a[__i + 1];
  return __r;
}
static __inline__ uint32x4x2_t vzipq_u32(uint32x4_t __a, uint32x4_t __b) {
  uint32x4x2_t __r;
  __r.val[0] = vzip1q_u32(__a, __b);
  __r.val[1] = vzip2q_u32(__a, __b);
  return __r;
}
static __inline__ uint32x4x2_t vuzpq_u32(uint32x4_t __a, uint32x4_t __b) {
  uint32x4x2_t __r;
  __r.val[0] = vuzp1q_u32(__a, __b);
  __r.val[1] = vuzp2q_u32(__a, __b);
  return __r;
}
static __inline__ uint32x4x2_t vtrnq_u32(uint32x4_t __a, uint32x4_t __b) {
  uint32x4x2_t __r;
  __r.val[0] = vtrn1q_u32(__a, __b);
  __r.val[1] = vtrn2q_u32(__a, __b);
  return __r;
}
static __inline__ uint32x4_t vrev64q_u32(uint32x4_t __a) {
  uint32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i - __i % 2 + 2 - 1 - __i % 2];
  return __r;
}
static __inline__ uint64x2_t vextq_u64(uint64x2_t __a, uint64x2_t __b, const int __k) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i + __k < 2 ? __a[__i + __k] : __b[__i + __k - 2];
  return __r;
}
static __inline__ uint64x2_t vzip1q_u64(uint64x2_t __a, uint64x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i % 2 ? __b[__i / 2] : __a[__i / 2];
  return __r;
}
static __inline__ uint64x2_t vzip2q_u64(uint64x2_t __a, uint64x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i % 2 ? __b[1 + __i / 2] : __a[1 + __i / 2];
  return __r;
}
static __inline__ uint64x2_t vuzp1q_u64(uint64x2_t __a, uint64x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i < 1 ? __a[2 * __i] : __b[2 * (__i - 1)];
  return __r;
}
static __inline__ uint64x2_t vuzp2q_u64(uint64x2_t __a, uint64x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i < 1 ? __a[2 * __i + 1] : __b[2 * (__i - 1) + 1];
  return __r;
}
static __inline__ uint64x2_t vtrn1q_u64(uint64x2_t __a, uint64x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i % 2 ? __b[__i - 1] : __a[__i];
  return __r;
}
static __inline__ uint64x2_t vtrn2q_u64(uint64x2_t __a, uint64x2_t __b) {
  uint64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i % 2 ? __b[__i] : __a[__i + 1];
  return __r;
}
static __inline__ float32x4_t vextq_f32(float32x4_t __a, float32x4_t __b, const int __k) {
  float32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i + __k < 4 ? __a[__i + __k] : __b[__i + __k - 4];
  return __r;
}
static __inline__ float32x4_t vzip1q_f32(float32x4_t __a, float32x4_t __b) {
  float32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i % 2 ? __b[__i / 2] : __a[__i / 2];
  return __r;
}
static __inline__ float32x4_t vzip2q_f32(float32x4_t __a, float32x4_t __b) {
  float32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i % 2 ? __b[2 + __i / 2] : __a[2 + __i / 2];
  return __r;
}
static __inline__ float32x4_t vuzp1q_f32(float32x4_t __a, float32x4_t __b) {
  float32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i < 2 ? __a[2 * __i] : __b[2 * (__i - 2)];
  return __r;
}
static __inline__ float32x4_t vuzp2q_f32(float32x4_t __a, float32x4_t __b) {
  float32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i < 2 ? __a[2 * __i + 1] : __b[2 * (__i - 2) + 1];
  return __r;
}
static __inline__ float32x4_t vtrn1q_f32(float32x4_t __a, float32x4_t __b) {
  float32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i % 2 ? __b[__i - 1] : __a[__i];
  return __r;
}
static __inline__ float32x4_t vtrn2q_f32(float32x4_t __a, float32x4_t __b) {
  float32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __i % 2 ? __b[__i] : __a[__i + 1];
  return __r;
}
static __inline__ float32x4x2_t vzipq_f32(float32x4_t __a, float32x4_t __b) {
  float32x4x2_t __r;
  __r.val[0] = vzip1q_f32(__a, __b);
  __r.val[1] = vzip2q_f32(__a, __b);
  return __r;
}
static __inline__ float32x4x2_t vuzpq_f32(float32x4_t __a, float32x4_t __b) {
  float32x4x2_t __r;
  __r.val[0] = vuzp1q_f32(__a, __b);
  __r.val[1] = vuzp2q_f32(__a, __b);
  return __r;
}
static __inline__ float32x4x2_t vtrnq_f32(float32x4_t __a, float32x4_t __b) {
  float32x4x2_t __r;
  __r.val[0] = vtrn1q_f32(__a, __b);
  __r.val[1] = vtrn2q_f32(__a, __b);
  return __r;
}
static __inline__ float32x4_t vrev64q_f32(float32x4_t __a) {
  float32x4_t __r;
  for (int __i = 0; __i < 4; __i++) __r[__i] = __a[__i - __i % 2 + 2 - 1 - __i % 2];
  return __r;
}
static __inline__ float64x2_t vextq_f64(float64x2_t __a, float64x2_t __b, const int __k) {
  float64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i + __k < 2 ? __a[__i + __k] : __b[__i + __k - 2];
  return __r;
}
static __inline__ float64x2_t vzip1q_f64(float64x2_t __a, float64x2_t __b) {
  float64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i % 2 ? __b[__i / 2] : __a[__i / 2];
  return __r;
}
static __inline__ float64x2_t vzip2q_f64(float64x2_t __a, float64x2_t __b) {
  float64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i % 2 ? __b[1 + __i / 2] : __a[1 + __i / 2];
  return __r;
}
static __inline__ float64x2_t vuzp1q_f64(float64x2_t __a, float64x2_t __b) {
  float64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i < 1 ? __a[2 * __i] : __b[2 * (__i - 1)];
  return __r;
}
static __inline__ float64x2_t vuzp2q_f64(float64x2_t __a, float64x2_t __b) {
  float64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i < 1 ? __a[2 * __i + 1] : __b[2 * (__i - 1) + 1];
  return __r;
}
static __inline__ float64x2_t vtrn1q_f64(float64x2_t __a, float64x2_t __b) {
  float64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i % 2 ? __b[__i - 1] : __a[__i];
  return __r;
}
static __inline__ float64x2_t vtrn2q_f64(float64x2_t __a, float64x2_t __b) {
  float64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i % 2 ? __b[__i] : __a[__i + 1];
  return __r;
}
static __inline__ poly8x16_t vextq_p8(poly8x16_t __a, poly8x16_t __b, const int __k) {
  poly8x16_t __r;
  for (int __i = 0; __i < 16; __i++)
    __r[__i] = __i + __k < 16 ? __a[__i + __k] : __b[__i + __k - 16];
  return __r;
}
static __inline__ poly8x16_t vzip1q_p8(poly8x16_t __a, poly8x16_t __b) {
  poly8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __i % 2 ? __b[__i / 2] : __a[__i / 2];
  return __r;
}
static __inline__ poly8x16_t vzip2q_p8(poly8x16_t __a, poly8x16_t __b) {
  poly8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __i % 2 ? __b[8 + __i / 2] : __a[8 + __i / 2];
  return __r;
}
static __inline__ poly8x16_t vuzp1q_p8(poly8x16_t __a, poly8x16_t __b) {
  poly8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __i < 8 ? __a[2 * __i] : __b[2 * (__i - 8)];
  return __r;
}
static __inline__ poly8x16_t vuzp2q_p8(poly8x16_t __a, poly8x16_t __b) {
  poly8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __i < 8 ? __a[2 * __i + 1] : __b[2 * (__i - 8) + 1];
  return __r;
}
static __inline__ poly8x16_t vtrn1q_p8(poly8x16_t __a, poly8x16_t __b) {
  poly8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __i % 2 ? __b[__i - 1] : __a[__i];
  return __r;
}
static __inline__ poly8x16_t vtrn2q_p8(poly8x16_t __a, poly8x16_t __b) {
  poly8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __i % 2 ? __b[__i] : __a[__i + 1];
  return __r;
}
static __inline__ poly8x16x2_t vzipq_p8(poly8x16_t __a, poly8x16_t __b) {
  poly8x16x2_t __r;
  __r.val[0] = vzip1q_p8(__a, __b);
  __r.val[1] = vzip2q_p8(__a, __b);
  return __r;
}
static __inline__ poly8x16x2_t vuzpq_p8(poly8x16_t __a, poly8x16_t __b) {
  poly8x16x2_t __r;
  __r.val[0] = vuzp1q_p8(__a, __b);
  __r.val[1] = vuzp2q_p8(__a, __b);
  return __r;
}
static __inline__ poly8x16x2_t vtrnq_p8(poly8x16_t __a, poly8x16_t __b) {
  poly8x16x2_t __r;
  __r.val[0] = vtrn1q_p8(__a, __b);
  __r.val[1] = vtrn2q_p8(__a, __b);
  return __r;
}
static __inline__ poly8x16_t vrev16q_p8(poly8x16_t __a) {
  poly8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __a[__i - __i % 2 + 2 - 1 - __i % 2];
  return __r;
}
static __inline__ poly8x16_t vrev32q_p8(poly8x16_t __a) {
  poly8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __a[__i - __i % 4 + 4 - 1 - __i % 4];
  return __r;
}
static __inline__ poly8x16_t vrev64q_p8(poly8x16_t __a) {
  poly8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __a[__i - __i % 8 + 8 - 1 - __i % 8];
  return __r;
}
static __inline__ poly16x8_t vextq_p16(poly16x8_t __a, poly16x8_t __b, const int __k) {
  poly16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i + __k < 8 ? __a[__i + __k] : __b[__i + __k - 8];
  return __r;
}
static __inline__ poly16x8_t vzip1q_p16(poly16x8_t __a, poly16x8_t __b) {
  poly16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i % 2 ? __b[__i / 2] : __a[__i / 2];
  return __r;
}
static __inline__ poly16x8_t vzip2q_p16(poly16x8_t __a, poly16x8_t __b) {
  poly16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i % 2 ? __b[4 + __i / 2] : __a[4 + __i / 2];
  return __r;
}
static __inline__ poly16x8_t vuzp1q_p16(poly16x8_t __a, poly16x8_t __b) {
  poly16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i < 4 ? __a[2 * __i] : __b[2 * (__i - 4)];
  return __r;
}
static __inline__ poly16x8_t vuzp2q_p16(poly16x8_t __a, poly16x8_t __b) {
  poly16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i < 4 ? __a[2 * __i + 1] : __b[2 * (__i - 4) + 1];
  return __r;
}
static __inline__ poly16x8_t vtrn1q_p16(poly16x8_t __a, poly16x8_t __b) {
  poly16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i % 2 ? __b[__i - 1] : __a[__i];
  return __r;
}
static __inline__ poly16x8_t vtrn2q_p16(poly16x8_t __a, poly16x8_t __b) {
  poly16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __i % 2 ? __b[__i] : __a[__i + 1];
  return __r;
}
static __inline__ poly16x8x2_t vzipq_p16(poly16x8_t __a, poly16x8_t __b) {
  poly16x8x2_t __r;
  __r.val[0] = vzip1q_p16(__a, __b);
  __r.val[1] = vzip2q_p16(__a, __b);
  return __r;
}
static __inline__ poly16x8x2_t vuzpq_p16(poly16x8_t __a, poly16x8_t __b) {
  poly16x8x2_t __r;
  __r.val[0] = vuzp1q_p16(__a, __b);
  __r.val[1] = vuzp2q_p16(__a, __b);
  return __r;
}
static __inline__ poly16x8x2_t vtrnq_p16(poly16x8_t __a, poly16x8_t __b) {
  poly16x8x2_t __r;
  __r.val[0] = vtrn1q_p16(__a, __b);
  __r.val[1] = vtrn2q_p16(__a, __b);
  return __r;
}
static __inline__ poly16x8_t vrev32q_p16(poly16x8_t __a) {
  poly16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i - __i % 2 + 2 - 1 - __i % 2];
  return __r;
}
static __inline__ poly16x8_t vrev64q_p16(poly16x8_t __a) {
  poly16x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __a[__i - __i % 4 + 4 - 1 - __i % 4];
  return __r;
}
static __inline__ poly64x2_t vextq_p64(poly64x2_t __a, poly64x2_t __b, const int __k) {
  poly64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i + __k < 2 ? __a[__i + __k] : __b[__i + __k - 2];
  return __r;
}
static __inline__ poly64x2_t vzip1q_p64(poly64x2_t __a, poly64x2_t __b) {
  poly64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i % 2 ? __b[__i / 2] : __a[__i / 2];
  return __r;
}
static __inline__ poly64x2_t vzip2q_p64(poly64x2_t __a, poly64x2_t __b) {
  poly64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i % 2 ? __b[1 + __i / 2] : __a[1 + __i / 2];
  return __r;
}
static __inline__ poly64x2_t vuzp1q_p64(poly64x2_t __a, poly64x2_t __b) {
  poly64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i < 1 ? __a[2 * __i] : __b[2 * (__i - 1)];
  return __r;
}
static __inline__ poly64x2_t vuzp2q_p64(poly64x2_t __a, poly64x2_t __b) {
  poly64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i < 1 ? __a[2 * __i + 1] : __b[2 * (__i - 1) + 1];
  return __r;
}
static __inline__ poly64x2_t vtrn1q_p64(poly64x2_t __a, poly64x2_t __b) {
  poly64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i % 2 ? __b[__i - 1] : __a[__i];
  return __r;
}
static __inline__ poly64x2_t vtrn2q_p64(poly64x2_t __a, poly64x2_t __b) {
  poly64x2_t __r;
  for (int __i = 0; __i < 2; __i++) __r[__i] = __i % 2 ? __b[__i] : __a[__i + 1];
  return __r;
}
static __inline__ uint8x8_t vqtbl1_u8(uint8x16_t __t, uint8x8_t __x) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __x[__i] < 16 ? __t[__x[__i]] : 0;
  return __r;
}
static __inline__ uint8x8_t vqtbx1_u8(uint8x8_t __lo, uint8x16_t __t, uint8x8_t __x) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __x[__i] < 16 ? __t[__x[__i]] : __lo[__i];
  return __r;
}
static __inline__ int8x8_t vqtbl1_s8(int8x16_t __t, uint8x8_t __x) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __x[__i] < 16 ? __t[__x[__i]] : 0;
  return __r;
}
static __inline__ int8x8_t vqtbx1_s8(int8x8_t __lo, int8x16_t __t, uint8x8_t __x) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __x[__i] < 16 ? __t[__x[__i]] : __lo[__i];
  return __r;
}
static __inline__ poly8x8_t vqtbl1_p8(poly8x16_t __t, uint8x8_t __x) {
  poly8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __x[__i] < 16 ? __t[__x[__i]] : 0;
  return __r;
}
static __inline__ poly8x8_t vqtbx1_p8(poly8x8_t __lo, poly8x16_t __t, uint8x8_t __x) {
  poly8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = __x[__i] < 16 ? __t[__x[__i]] : __lo[__i];
  return __r;
}
static __inline__ uint8x16_t vqtbl1q_u8(uint8x16_t __t, uint8x16_t __x) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __x[__i] < 16 ? __t[__x[__i]] : 0;
  return __r;
}
static __inline__ uint8x16_t vqtbx1q_u8(uint8x16_t __lo, uint8x16_t __t, uint8x16_t __x) {
  uint8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __x[__i] < 16 ? __t[__x[__i]] : __lo[__i];
  return __r;
}
static __inline__ int8x16_t vqtbl1q_s8(int8x16_t __t, uint8x16_t __x) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __x[__i] < 16 ? __t[__x[__i]] : 0;
  return __r;
}
static __inline__ int8x16_t vqtbx1q_s8(int8x16_t __lo, int8x16_t __t, uint8x16_t __x) {
  int8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __x[__i] < 16 ? __t[__x[__i]] : __lo[__i];
  return __r;
}
static __inline__ poly8x16_t vqtbl1q_p8(poly8x16_t __t, uint8x16_t __x) {
  poly8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __x[__i] < 16 ? __t[__x[__i]] : 0;
  return __r;
}
static __inline__ poly8x16_t vqtbx1q_p8(poly8x16_t __lo, poly8x16_t __t, uint8x16_t __x) {
  poly8x16_t __r;
  for (int __i = 0; __i < 16; __i++) __r[__i] = __x[__i] < 16 ? __t[__x[__i]] : __lo[__i];
  return __r;
}
static __inline__ uint8x8_t vtbl1_u8(uint8x8_t __t, uint8x8_t __x) {
  uint8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint8_t)__x[__i] < 8 ? __t[(uint8_t)__x[__i]] : 0;
  return __r;
}
static __inline__ int8x8_t vtbl1_s8(int8x8_t __t, int8x8_t __x) {
  int8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint8_t)__x[__i] < 8 ? __t[(uint8_t)__x[__i]] : 0;
  return __r;
}
static __inline__ poly8x8_t vtbl1_p8(poly8x8_t __t, uint8x8_t __x) {
  poly8x8_t __r;
  for (int __i = 0; __i < 8; __i++) __r[__i] = (uint8_t)__x[__i] < 8 ? __t[(uint8_t)__x[__i]] : 0;
  return __r;
}

#endif
