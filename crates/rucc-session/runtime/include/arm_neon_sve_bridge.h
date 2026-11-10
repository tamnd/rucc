/* arm_neon_sve_bridge.h, which joins the Advanced SIMD types and the SVE types.
 *
 * gcc 14 and later define `__ARM_NEON_SVE_BRIDGE` on every AArch64 target, and this compiler does
 * too when `__GNUC__` is 14 or more. The macro says that this header is there. gcc's header
 * includes `<arm_neon.h>` and `<arm_sve.h>` and then declares `svset_neonq`, `svget_neonq` and
 * `svdup_neonq` through a pragma. Each of them moves a 128-bit vector into or out of an SVE vector,
 * and a call to one is an error without SVE.
 *
 * This header includes the same two headers and declares no function, for the reason that
 * `<arm_sve.h>` gives. A program can include it and name the types of both headers. */

#ifndef __RUCC_ARM_NEON_SVE_BRIDGE_H
#define __RUCC_ARM_NEON_SVE_BRIDGE_H

#include <arm_neon.h>
#include <arm_sve.h>

#endif
