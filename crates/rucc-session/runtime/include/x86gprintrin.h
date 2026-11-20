/* x86gprintrin.h, the intrinsics over general purpose registers.
 *
 * gcc's <immintrin.h> includes this, and so does this compiler's. It holds the names that work on
 * integers in ordinary registers: the population count, the leading and trailing zero counts, the
 * bit manipulation of BMI1 and BMI2, `_xgetbv` and the shadow stack instructions. Each header
 * under it builds each function for its own extension.
 */

#ifndef __RUCC_X86GPRINTRIN_H
#define __RUCC_X86GPRINTRIN_H

#include <popcntintrin.h>
#include <lzcntintrin.h>
#include <bmiintrin.h>
#include <bmi2intrin.h>
#include <xsaveintrin.h>
#include <cetintrin.h>

#endif /* __RUCC_X86GPRINTRIN_H */
