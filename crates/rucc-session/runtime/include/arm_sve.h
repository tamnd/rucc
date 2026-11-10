/* arm_sve.h, the types of the AArch64 Scalable Vector Extension.
 *
 * What is here is the types and the two enumerations of gcc's header, and none of the intrinsics.
 * gcc writes its header as one pragma, and the compiler then declares the types and the functions.
 * gcc does that on every AArch64 target, with SVE or without it. Without SVE, a call to one of the
 * functions is an error. This compiler does not build SVE code, so it declares no function. A
 * call to one is a call to an undeclared function. An SVE vector is an incomplete type here, so
 * the call fails as it does in gcc without SVE.
 *
 * Each vector type is a name the compiler has before any header is read, such as `__SVInt8_t`.
 * A vector type has no size that is known when the program is compiled. Here each one is an
 * incomplete type, so a prototype can name it and `sizeof` cannot. Each tuple type is an
 * incomplete structure for the same reason.
 *
 * The values of `enum svpattern` and `enum svprfop` are the values that gcc gives them. The
 * header is included by `<arm_neon_sve_bridge.h>`, which gcc 14 and later promise with
 * `__ARM_NEON_SVE_BRIDGE`. */

#ifndef __RUCC_ARM_SVE_H
#define __RUCC_ARM_SVE_H

#if !defined(__aarch64__)
#error "arm_sve.h is for AArch64"
#endif

#include <stdint.h>

typedef __bf16 bfloat16_t;
typedef __fp16 float16_t;
typedef float float32_t;
typedef double float64_t;

typedef __SVInt8_t svint8_t;
typedef __SVInt16_t svint16_t;
typedef __SVInt32_t svint32_t;
typedef __SVInt64_t svint64_t;
typedef __SVUint8_t svuint8_t;
typedef __SVUint16_t svuint16_t;
typedef __SVUint32_t svuint32_t;
typedef __SVUint64_t svuint64_t;
typedef __SVFloat16_t svfloat16_t;
typedef __SVFloat32_t svfloat32_t;
typedef __SVFloat64_t svfloat64_t;
typedef __SVBfloat16_t svbfloat16_t;
typedef __SVBool_t svbool_t;

typedef struct __rucc_svint8x2_t svint8x2_t;
typedef struct __rucc_svint8x3_t svint8x3_t;
typedef struct __rucc_svint8x4_t svint8x4_t;
typedef struct __rucc_svint16x2_t svint16x2_t;
typedef struct __rucc_svint16x3_t svint16x3_t;
typedef struct __rucc_svint16x4_t svint16x4_t;
typedef struct __rucc_svint32x2_t svint32x2_t;
typedef struct __rucc_svint32x3_t svint32x3_t;
typedef struct __rucc_svint32x4_t svint32x4_t;
typedef struct __rucc_svint64x2_t svint64x2_t;
typedef struct __rucc_svint64x3_t svint64x3_t;
typedef struct __rucc_svint64x4_t svint64x4_t;
typedef struct __rucc_svuint8x2_t svuint8x2_t;
typedef struct __rucc_svuint8x3_t svuint8x3_t;
typedef struct __rucc_svuint8x4_t svuint8x4_t;
typedef struct __rucc_svuint16x2_t svuint16x2_t;
typedef struct __rucc_svuint16x3_t svuint16x3_t;
typedef struct __rucc_svuint16x4_t svuint16x4_t;
typedef struct __rucc_svuint32x2_t svuint32x2_t;
typedef struct __rucc_svuint32x3_t svuint32x3_t;
typedef struct __rucc_svuint32x4_t svuint32x4_t;
typedef struct __rucc_svuint64x2_t svuint64x2_t;
typedef struct __rucc_svuint64x3_t svuint64x3_t;
typedef struct __rucc_svuint64x4_t svuint64x4_t;
typedef struct __rucc_svfloat16x2_t svfloat16x2_t;
typedef struct __rucc_svfloat16x3_t svfloat16x3_t;
typedef struct __rucc_svfloat16x4_t svfloat16x4_t;
typedef struct __rucc_svfloat32x2_t svfloat32x2_t;
typedef struct __rucc_svfloat32x3_t svfloat32x3_t;
typedef struct __rucc_svfloat32x4_t svfloat32x4_t;
typedef struct __rucc_svfloat64x2_t svfloat64x2_t;
typedef struct __rucc_svfloat64x3_t svfloat64x3_t;
typedef struct __rucc_svfloat64x4_t svfloat64x4_t;
typedef struct __rucc_svbfloat16x2_t svbfloat16x2_t;
typedef struct __rucc_svbfloat16x3_t svbfloat16x3_t;
typedef struct __rucc_svbfloat16x4_t svbfloat16x4_t;

enum svpattern {
  SV_POW2 = 0,
  SV_VL1 = 1,
  SV_VL2 = 2,
  SV_VL3 = 3,
  SV_VL4 = 4,
  SV_VL5 = 5,
  SV_VL6 = 6,
  SV_VL7 = 7,
  SV_VL8 = 8,
  SV_VL16 = 9,
  SV_VL32 = 10,
  SV_VL64 = 11,
  SV_VL128 = 12,
  SV_VL256 = 13,
  SV_MUL4 = 29,
  SV_MUL3 = 30,
  SV_ALL = 31
};

enum svprfop {
  SV_PLDL1KEEP = 0,
  SV_PLDL1STRM = 1,
  SV_PLDL2KEEP = 2,
  SV_PLDL2STRM = 3,
  SV_PLDL3KEEP = 4,
  SV_PLDL3STRM = 5,
  SV_PSTL1KEEP = 8,
  SV_PSTL1STRM = 9,
  SV_PSTL2KEEP = 10,
  SV_PSTL2STRM = 11,
  SV_PSTL3KEEP = 12,
  SV_PSTL3STRM = 13
};

#endif
