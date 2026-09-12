/* mm_malloc.h, aligned allocation for the vector intrinsics.
 *
 * A vector load that names an aligned address faults when the address is not aligned, so a
 * program that allocates its own buffers needs a way to ask for one. That is the whole of
 * this header. It is two functions and they are not vector operations themselves, which is
 * why they live apart from `<xmmintrin.h>` rather than inside it.
 *
 * Memory from `_mm_malloc` is freed by `_mm_free` and by nothing else. On this target both
 * are built on the ordinary allocator, so `free` would work too, but a program that relies
 * on that stops working on a target where they are not the same allocator. */

#ifndef __RUCC_MM_MALLOC_H
#define __RUCC_MM_MALLOC_H

#include <stddef.h>

/* Declared here rather than pulled in from `<stdlib.h>`, because a program that includes a
 * vector header has not asked for the whole of the standard library and should not get it.
 * Both spellings match the ones in `<stdlib.h>`, so including both headers in either order
 * is two declarations of the same function and not a conflict. */
extern int posix_memalign(void **__memptr, size_t __alignment, size_t __size);
extern void *malloc(size_t __size);
extern void free(void *__ptr);

/* `__size` bytes aligned to `__align`, or a null pointer if there are not that many.
 *
 * `posix_memalign` asks for an alignment that is a power of two and at least the width of a
 * pointer, and refuses anything else. Both of the adjustments below are there to hand it a
 * request it will accept rather than to fail on one it would have refused: an alignment of
 * one is no alignment at all and is the plain allocator's job, and an alignment smaller than
 * a pointer is already what the plain allocator gives. An alignment that is not a power of
 * two is left alone and the failure is reported, since there is no sensible reading of it. */
static __inline__ void *__attribute__((__always_inline__))
_mm_malloc(size_t __size, size_t __align)
{
  void *__answer;
  if (__align == 1)
    return malloc(__size);
  if ((__align & (__align - 1)) == 0 && __align < sizeof(void *))
    __align = sizeof(void *);
  if (posix_memalign(&__answer, __align, __size) != 0)
    return (void *)0;
  return __answer;
}

/* Gives back memory that came from `_mm_malloc`. A null pointer is allowed and does nothing,
 * which is `free`'s own rule. */
static __inline__ void __attribute__((__always_inline__)) _mm_free(void *__ptr)
{
  free(__ptr);
}

#endif
