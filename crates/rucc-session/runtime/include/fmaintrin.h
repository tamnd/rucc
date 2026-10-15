/* fmaintrin.h, the fused multiply and add intrinsics.
 *
 * Reached through <immintrin.h> the way gcc reaches it. This has the 32 names gcc 16.2.0 has in its
 * own fmaintrin.h. Each one multiplies A by B and adds C or subtracts it, with one rounding at the
 * end. Each function is built for `fma`, and a call from a function that is not built for it is
 * refused the way gcc refuses it.
 *
 * Each function is the `213` form of the instruction in inline assembly through memory, the way
 * <avxintrin.h> does it. A goes in register 0, B in register 1 and C in register 2, and the
 * instruction leaves A times B plus C in register 0. A scalar form keeps the upper lanes of A, as
 * gcc does. The 32 byte forms end with `vzeroupper` for the reason <avxintrin.h> gives. See
 * `tamnd/rucc#2322`.
 */

#ifndef __RUCC_FMAINTRIN_H
#define __RUCC_FMAINTRIN_H

#include <avxintrin.h>

/* The shape of the assembly. `__w` is `x` for a 16 byte vector and `y` for a 32 byte one, and
 * `__end` is what comes after the store. */
#define __RUCC_FMA(__insn, __w, __end, __answer, __a, __b, __c)                                    \
  __asm__("vmovdqu (%1), %%" __w "mm0\n\tvmovdqu (%2), %%" __w "mm1\n\tvmovdqu (%3), %%" __w       \
          "mm2\n\t" __insn " %%" __w "mm2, %%" __w "mm1, %%" __w "mm0\n\tvmovdqu %%" __w           \
          "mm0, (%0)" __end                                                                        \
          :                                                                                        \
          : "r"(&(__answer)), "r"(&(__a)), "r"(&(__b)), "r"(&(__c))                                \
          : "xmm0", "xmm1", "xmm2", "memory")

#define __RUCC_FMA_128(__insn, __answer, __a, __b, __c)                                            \
  __RUCC_FMA(__insn, "x", "", __answer, __a, __b, __c)
#define __RUCC_FMA_256(__insn, __answer, __a, __b, __c)                                            \
  __RUCC_FMA(__insn, "y", "\n\tvzeroupper", __answer, __a, __b, __c)

/* A times B plus C. The scalar forms do it in the lowest lane only. */

static __inline__ __m128d __attribute__((__always_inline__, __target__("fma")))
_mm_fmadd_pd(__m128d __A, __m128d __B, __m128d __C)
{
  __m128d __R;
  __RUCC_FMA_128("vfmadd213pd", __R, __A, __B, __C);
  return __R;
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("fma")))
_mm256_fmadd_pd(__m256d __A, __m256d __B, __m256d __C)
{
  __m256d __R;
  __RUCC_FMA_256("vfmadd213pd", __R, __A, __B, __C);
  return __R;
}

static __inline__ __m128 __attribute__((__always_inline__, __target__("fma")))
_mm_fmadd_ps(__m128 __A, __m128 __B, __m128 __C)
{
  __m128 __R;
  __RUCC_FMA_128("vfmadd213ps", __R, __A, __B, __C);
  return __R;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("fma")))
_mm256_fmadd_ps(__m256 __A, __m256 __B, __m256 __C)
{
  __m256 __R;
  __RUCC_FMA_256("vfmadd213ps", __R, __A, __B, __C);
  return __R;
}

static __inline__ __m128d __attribute__((__always_inline__, __target__("fma")))
_mm_fmadd_sd(__m128d __A, __m128d __B, __m128d __C)
{
  __m128d __R;
  __RUCC_FMA_128("vfmadd213sd", __R, __A, __B, __C);
  return __R;
}

static __inline__ __m128 __attribute__((__always_inline__, __target__("fma")))
_mm_fmadd_ss(__m128 __A, __m128 __B, __m128 __C)
{
  __m128 __R;
  __RUCC_FMA_128("vfmadd213ss", __R, __A, __B, __C);
  return __R;
}

/* A times B minus C. The scalar forms do it in the lowest lane only. */

static __inline__ __m128d __attribute__((__always_inline__, __target__("fma")))
_mm_fmsub_pd(__m128d __A, __m128d __B, __m128d __C)
{
  __m128d __R;
  __RUCC_FMA_128("vfmsub213pd", __R, __A, __B, __C);
  return __R;
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("fma")))
_mm256_fmsub_pd(__m256d __A, __m256d __B, __m256d __C)
{
  __m256d __R;
  __RUCC_FMA_256("vfmsub213pd", __R, __A, __B, __C);
  return __R;
}

static __inline__ __m128 __attribute__((__always_inline__, __target__("fma")))
_mm_fmsub_ps(__m128 __A, __m128 __B, __m128 __C)
{
  __m128 __R;
  __RUCC_FMA_128("vfmsub213ps", __R, __A, __B, __C);
  return __R;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("fma")))
_mm256_fmsub_ps(__m256 __A, __m256 __B, __m256 __C)
{
  __m256 __R;
  __RUCC_FMA_256("vfmsub213ps", __R, __A, __B, __C);
  return __R;
}

static __inline__ __m128d __attribute__((__always_inline__, __target__("fma")))
_mm_fmsub_sd(__m128d __A, __m128d __B, __m128d __C)
{
  __m128d __R;
  __RUCC_FMA_128("vfmsub213sd", __R, __A, __B, __C);
  return __R;
}

static __inline__ __m128 __attribute__((__always_inline__, __target__("fma")))
_mm_fmsub_ss(__m128 __A, __m128 __B, __m128 __C)
{
  __m128 __R;
  __RUCC_FMA_128("vfmsub213ss", __R, __A, __B, __C);
  return __R;
}

/* C minus A times B. The scalar forms do it in the lowest lane only. */

static __inline__ __m128d __attribute__((__always_inline__, __target__("fma")))
_mm_fnmadd_pd(__m128d __A, __m128d __B, __m128d __C)
{
  __m128d __R;
  __RUCC_FMA_128("vfnmadd213pd", __R, __A, __B, __C);
  return __R;
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("fma")))
_mm256_fnmadd_pd(__m256d __A, __m256d __B, __m256d __C)
{
  __m256d __R;
  __RUCC_FMA_256("vfnmadd213pd", __R, __A, __B, __C);
  return __R;
}

static __inline__ __m128 __attribute__((__always_inline__, __target__("fma")))
_mm_fnmadd_ps(__m128 __A, __m128 __B, __m128 __C)
{
  __m128 __R;
  __RUCC_FMA_128("vfnmadd213ps", __R, __A, __B, __C);
  return __R;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("fma")))
_mm256_fnmadd_ps(__m256 __A, __m256 __B, __m256 __C)
{
  __m256 __R;
  __RUCC_FMA_256("vfnmadd213ps", __R, __A, __B, __C);
  return __R;
}

static __inline__ __m128d __attribute__((__always_inline__, __target__("fma")))
_mm_fnmadd_sd(__m128d __A, __m128d __B, __m128d __C)
{
  __m128d __R;
  __RUCC_FMA_128("vfnmadd213sd", __R, __A, __B, __C);
  return __R;
}

static __inline__ __m128 __attribute__((__always_inline__, __target__("fma")))
_mm_fnmadd_ss(__m128 __A, __m128 __B, __m128 __C)
{
  __m128 __R;
  __RUCC_FMA_128("vfnmadd213ss", __R, __A, __B, __C);
  return __R;
}

/* minus A times B minus C. The scalar forms do it in the lowest lane only. */

static __inline__ __m128d __attribute__((__always_inline__, __target__("fma")))
_mm_fnmsub_pd(__m128d __A, __m128d __B, __m128d __C)
{
  __m128d __R;
  __RUCC_FMA_128("vfnmsub213pd", __R, __A, __B, __C);
  return __R;
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("fma")))
_mm256_fnmsub_pd(__m256d __A, __m256d __B, __m256d __C)
{
  __m256d __R;
  __RUCC_FMA_256("vfnmsub213pd", __R, __A, __B, __C);
  return __R;
}

static __inline__ __m128 __attribute__((__always_inline__, __target__("fma")))
_mm_fnmsub_ps(__m128 __A, __m128 __B, __m128 __C)
{
  __m128 __R;
  __RUCC_FMA_128("vfnmsub213ps", __R, __A, __B, __C);
  return __R;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("fma")))
_mm256_fnmsub_ps(__m256 __A, __m256 __B, __m256 __C)
{
  __m256 __R;
  __RUCC_FMA_256("vfnmsub213ps", __R, __A, __B, __C);
  return __R;
}

static __inline__ __m128d __attribute__((__always_inline__, __target__("fma")))
_mm_fnmsub_sd(__m128d __A, __m128d __B, __m128d __C)
{
  __m128d __R;
  __RUCC_FMA_128("vfnmsub213sd", __R, __A, __B, __C);
  return __R;
}

static __inline__ __m128 __attribute__((__always_inline__, __target__("fma")))
_mm_fnmsub_ss(__m128 __A, __m128 __B, __m128 __C)
{
  __m128 __R;
  __RUCC_FMA_128("vfnmsub213ss", __R, __A, __B, __C);
  return __R;
}

/* A times B, minus C in the even lanes and plus C in the odd lanes. */

static __inline__ __m128d __attribute__((__always_inline__, __target__("fma")))
_mm_fmaddsub_pd(__m128d __A, __m128d __B, __m128d __C)
{
  __m128d __R;
  __RUCC_FMA_128("vfmaddsub213pd", __R, __A, __B, __C);
  return __R;
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("fma")))
_mm256_fmaddsub_pd(__m256d __A, __m256d __B, __m256d __C)
{
  __m256d __R;
  __RUCC_FMA_256("vfmaddsub213pd", __R, __A, __B, __C);
  return __R;
}

static __inline__ __m128 __attribute__((__always_inline__, __target__("fma")))
_mm_fmaddsub_ps(__m128 __A, __m128 __B, __m128 __C)
{
  __m128 __R;
  __RUCC_FMA_128("vfmaddsub213ps", __R, __A, __B, __C);
  return __R;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("fma")))
_mm256_fmaddsub_ps(__m256 __A, __m256 __B, __m256 __C)
{
  __m256 __R;
  __RUCC_FMA_256("vfmaddsub213ps", __R, __A, __B, __C);
  return __R;
}

/* A times B, plus C in the even lanes and minus C in the odd lanes. */

static __inline__ __m128d __attribute__((__always_inline__, __target__("fma")))
_mm_fmsubadd_pd(__m128d __A, __m128d __B, __m128d __C)
{
  __m128d __R;
  __RUCC_FMA_128("vfmsubadd213pd", __R, __A, __B, __C);
  return __R;
}

static __inline__ __m256d __attribute__((__always_inline__, __target__("fma")))
_mm256_fmsubadd_pd(__m256d __A, __m256d __B, __m256d __C)
{
  __m256d __R;
  __RUCC_FMA_256("vfmsubadd213pd", __R, __A, __B, __C);
  return __R;
}

static __inline__ __m128 __attribute__((__always_inline__, __target__("fma")))
_mm_fmsubadd_ps(__m128 __A, __m128 __B, __m128 __C)
{
  __m128 __R;
  __RUCC_FMA_128("vfmsubadd213ps", __R, __A, __B, __C);
  return __R;
}

static __inline__ __m256 __attribute__((__always_inline__, __target__("fma")))
_mm256_fmsubadd_ps(__m256 __A, __m256 __B, __m256 __C)
{
  __m256 __R;
  __RUCC_FMA_256("vfmsubadd213ps", __R, __A, __B, __C);
  return __R;
}

#endif /* __RUCC_FMAINTRIN_H */
